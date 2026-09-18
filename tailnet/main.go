// neige-tailnet is a private, userspace HTTPS listener, never a system daemon client.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"syscall"
	"time"

	"tailscale.com/client/local"
	"tailscale.com/ipn/ipnstate"
	"tailscale.com/logtail"
	"tailscale.com/tsnet"
)

const tailscaleVersion = "1.102.3"
const version = "0.1.0"

type tsnetNode struct {
	server *tsnet.Server
	client *local.Client
}

func (n *tsnetNode) status(ctx context.Context) (*ipnstate.Status, error) {
	return n.client.StatusWithoutPeers(ctx)
}
func (n *tsnetNode) login(ctx context.Context) error  { return n.client.StartLoginInteractive(ctx) }
func (n *tsnetNode) logout(ctx context.Context) error { return n.client.Logout(ctx) }
func (n *tsnetNode) lockEnabled(ctx context.Context) (bool, error) {
	status, err := n.client.TailnetLockStatus(ctx)
	if err != nil || status == nil {
		return true, errors.New("Tailnet Lock status unavailable")
	}
	return status.Enabled, nil
}
func (n *tsnetNode) certificate(ctx context.Context, name string) error {
	cert, key, err := n.client.CertPair(ctx, name)
	if err != nil {
		return err
	}
	pair, err := tls.X509KeyPair(cert, key)
	if err != nil {
		return err
	}
	leaf, err := x509.ParseCertificate(pair.Certificate[0])
	if err != nil {
		return err
	}
	if time.Now().Before(leaf.NotBefore) || !time.Now().Before(leaf.NotAfter) {
		return errors.New("HTTPS certificate is not current")
	}
	return leaf.VerifyHostname(name)
}
func (n *tsnetNode) listen() (net.Listener, error) { return n.server.ListenTLS("tcp", ":443") }

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "neige-tailnet: startup failed; check private configuration and state version")
		os.Exit(1)
	}
}
func run() error {
	stateDir := flag.String("state-dir", "", "private persistent node directory")
	control := flag.String("control-socket", "", "private control socket")
	upstream := flag.String("upstream-socket", "", "fixed restricted Unix ingress")
	hostname := flag.String("hostname", "neige", "private node name")
	enrollmentConfig := flag.String("enrollment-config", "", "private issuer configuration file")
	cleanupOnly := flag.Bool("cleanup-only", false, "clean pending auth keys without starting a node or listener")
	showVersion := flag.Bool("version", false, "print version")
	flag.Parse()
	if *showVersion {
		fmt.Println("neige-tailnet " + version)
		return nil
	}
	build, ok := debug.ReadBuildInfo()
	if !ok {
		return errors.New("missing helper build identity")
	}
	pinned := false
	for _, dependency := range build.Deps {
		if dependency.Path == "tailscale.com" {
			pinned = dependency.Version == "v"+tailscaleVersion && dependency.Replace == nil
		}
	}
	if !pinned {
		return errors.New("tsnet state version does not match the built module")
	}
	if runtime.GOOS != "linux" {
		return errors.New("only Linux is supported")
	}
	if !filepath.IsAbs(*stateDir) || !filepath.IsAbs(*control) || filepath.Dir(*control) != *stateDir {
		return errors.New("private absolute paths required")
	}
	target, err := fixedUpstream(*upstream)
	if err != nil {
		return err
	}
	if filepath.Dir(target) != *stateDir {
		return errors.New("upstream must stay in the node private directory")
	}
	// Standalone invocations have the same environment boundary as app spawns.
	os.Clearenv()
	os.Setenv("PATH", "/usr/bin:/bin")
	os.Setenv("LANG", "C.UTF-8")
	os.Setenv("HOME", *stateDir)
	syscall.Umask(0077)
	lock, err := lockState(*stateDir)
	if err != nil {
		return err
	}
	defer lock.Close()
	var enrollment *issuer
	if *enrollmentConfig != "" {
		enrollment, err = newIssuer(*stateDir, *enrollmentConfig)
		if err != nil {
			return err
		}
		defer enrollment.dir.Close()
		ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
		_, err = enrollment.cleanup(ctx, "", true)
		cancel()
		if err != nil {
			return err
		}
	}
	if *cleanupOnly {
		return nil
	}
	if err = checkStateVersion(*stateDir); err != nil {
		return err
	}
	if err = os.Remove(*control); err != nil && !os.IsNotExist(err) {
		return err
	}
	listener, err := net.Listen("unix", *control)
	if err != nil {
		return err
	}
	defer listener.Close()
	defer os.Remove(*control)
	if err = os.Chmod(*control, 0600); err != nil {
		return err
	}
	server := privateTailnetServer(*stateDir, *hostname)
	if err = server.Start(); err != nil {
		return err
	}
	defer server.Close()
	client, err := server.LocalClient()
	if err != nil {
		return err
	}
	service := newService(&tsnetNode{server, client}, target)
	service.issuer = enrollment
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	done := make(chan struct{})
	go func() { service.run(ctx); close(done) }()
	serveControl(ctx, listener, service)
	cancel()
	<-done
	service.closeIngress()
	return nil
}

func privateTailnetServer(stateDir, hostname string) *tsnet.Server {
	// Quiet callbacks alone do not stop tsnet's earlier logtail writes. Disable
	// new entries before Start; release builds also omit logtail/filch entirely
	// so pre-existing disk buffers cannot be drained by an uploader.
	logtail.Disable()
	log.SetOutput(io.Discard)
	quiet := func(string, ...any) {}
	return &tsnet.Server{Dir: filepath.Join(stateDir, "node"), Hostname: hostname, UserLogf: quiet, Logf: quiet}
}

func lockState(dir string) (*os.File, error) {
	if err := os.MkdirAll(dir, 0700); err != nil {
		return nil, err
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() || info.Mode().Perm() != 0700 {
		return nil, errors.New("state directory must be private")
	}
	file, err := os.OpenFile(filepath.Join(dir, "node.lock"), os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return nil, err
	}
	if err = syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		file.Close()
		return nil, err
	}
	return file, nil
}
func checkStateVersion(dir string) error {
	path := filepath.Join(dir, "state-version")
	previous, err := os.ReadFile(path)
	if err == nil {
		if string(previous) != tailscaleVersion {
			return errors.New("state needs a validated migration before using another tsnet version")
		}
		return nil
	}
	if !os.IsNotExist(err) {
		return err
	}
	// Do not guess the provenance of pre-existing identity state.
	if _, err = os.Stat(filepath.Join(dir, "node")); err == nil {
		return errors.New("unversioned node state")
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer file.Close()
	if _, err = file.WriteString(tailscaleVersion); err != nil {
		return err
	}
	return file.Sync()
}

package main

import (
	"encoding/json"
	"fmt"
	"net"
	"sync"
	"sync/atomic"
	"tailscale.com/net/netmon"
)

var interfaceSnapshot atomic.Pointer[[]netmon.Interface]
var registerInterfaces sync.Once

func configureInterfaces(raw string) string {
	var rows []struct {
		Name      string   `json:"name"`
		Index     int      `json:"index"`
		MTU       int      `json:"mtu"`
		Flags     uint     `json:"flags"`
		Addresses []string `json:"addresses"`
	}
	if err := json.Unmarshal([]byte(raw), &rows); err != nil {
		return failure(err)
	}
	interfaces := make([]netmon.Interface, 0, len(rows))
	for _, row := range rows {
		if row.Name == "" || row.Index <= 0 {
			return failure(fmt.Errorf("无效的 Android 网卡信息"))
		}
		addresses := make([]net.Addr, 0, len(row.Addresses))
		for _, address := range row.Addresses {
			ip, prefix, err := net.ParseCIDR(address)
			if err != nil {
				return failure(err)
			}
			prefix.IP = ip
			addresses = append(addresses, prefix)
		}
		interfaces = append(interfaces, netmon.Interface{Interface: &net.Interface{Name: row.Name, Index: row.Index, MTU: row.MTU, Flags: net.Flags(row.Flags)}, AltAddrs: addresses})
	}
	interfaceSnapshot.Store(&interfaces)
	registerInterfaces.Do(func() {
		netmon.RegisterInterfaceGetter(func() ([]netmon.Interface, error) { return *interfaceSnapshot.Load(), nil })
	})
	return encoded(map[string]any{"ok": true})
}

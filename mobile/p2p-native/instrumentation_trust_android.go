//go:build android && neige_instrumentation

package main

/*
#include <stdlib.h>
static const char *neige_instrumentation_ca(void) {
    return getenv("NEIGE_INSTRUMENTATION_CA");
}
*/
import "C"

import "os"

// Android c-shared startup does not import the Java process environment into
// Go. Only the isolated instrumentation build bridges this one fixture setting.
// crypto/x509 still verifies the certificate chain and original hostname.
func init() {
	if path := C.neige_instrumentation_ca(); path != nil {
		if err := os.Setenv("SSL_CERT_FILE", C.GoString(path)); err != nil {
			panic(err)
		}
	}
}

#define _GNU_SOURCE
#include <dlfcn.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Child-process-only libc boundary fixture. Go still invokes its real cgo
 * resolver; family hints and errors are observed at the getaddrinfo ABI.
 * AF_UNSPEC intentionally models aggregate success despite a failed family. */
int getaddrinfo(const char *name, const char *service,
                const struct addrinfo *hints, struct addrinfo **result) {
    typedef int (*original_fn)(const char *, const char *, const struct addrinfo *, struct addrinfo **);
    original_fn original = (original_fn)dlsym(RTLD_NEXT, "getaddrinfo");
    const char *mode = getenv("NEIGE_TEST_DIRECT_DNS_MODE");
    if (!mode || !name || strcmp(name, "example.com") != 0)
        return original(name, service, hints, result);
    int family = hints ? hints->ai_family : AF_UNSPEC;
    const char *path = getenv("NEIGE_TEST_DIRECT_DNS_TRACE");
    FILE *trace = path ? fopen(path, "a") : NULL;
    if (trace) { fprintf(trace, "%d\n", family); fclose(trace); }
    if (family == AF_INET6) {
        if (strcmp(mode, "AAAA_COLLAPSED") == 0) return EAI_NODATA;
        if (strcmp(mode, "AAAA_NODATA") == 0) return EAI_NONAME;
        if (strcmp(mode, "AAAA_SERVFAIL") == 0) return EAI_AGAIN;
        if (strcmp(mode, "AAAA_REFUSED") == 0) return EAI_FAIL;
    }
    if (family == AF_INET && strcmp(mode, "A_SERVFAIL") == 0) return EAI_AGAIN;
    if (family == AF_INET && strcmp(mode, "A_NODATA") == 0) return EAI_NONAME;
    struct addrinfo literal = hints ? *hints : (struct addrinfo){0};
    literal.ai_family = family == AF_INET6 ? AF_INET6 : AF_INET;
    literal.ai_flags = AI_NUMERICHOST;
    return original(literal.ai_family == AF_INET6 ? "fd00::8" : "192.168.1.8", service, &literal, result);
}

//go:build (android || linux) && cgo

#include "direct_dns_native.h"
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>

/* API29 symbols must not be hard-linked: the APK still loads on API26-28.
 * Signatures match NDK android/multinetwork.h. Linux compiles this same adapter
 * for child-process ABI fixtures only; production host DNS does not call it. */
typedef int (*query_fn)(uint64_t, const char *, int, int, uint32_t);
typedef int (*result_fn)(int, int *, uint8_t *, size_t);
typedef void (*cancel_fn)(int);
static pthread_once_t once = PTHREAD_ONCE_INIT;
static query_fn query;
static result_fn result;
static cancel_fn cancel_query;

static void load_resolver(void) {
    void *library = dlopen("libandroid.so", RTLD_NOW | RTLD_LOCAL);
    if (!library) return;
    query = (query_fn)dlsym(library, "android_res_nquery");
    result = (result_fn)dlsym(library, "android_res_nresult");
    cancel_query = (cancel_fn)dlsym(library, "android_res_cancel");
    if (!query || !result || !cancel_query) {
        query = NULL; result = NULL; cancel_query = NULL;
        dlclose(library);
    }
    /* Successful resolution retains one process-lifetime library reference. */
}

int neige_dns_query(const char *host, int type) {
    pthread_once(&once, load_resolver);
    if (!query) return -ENOSYS;
    /* NETWORK_UNSPECIFIED, class IN, flags 0: retain platform routing, cache,
     * private DNS, and retry policy. Never select a network or DNS server. */
    return query(0, host, 1, type, 0);
}
int neige_dns_result(int fd, int *rcode, uint8_t *answer, size_t length) {
    return result(fd, rcode, answer, length);
}
void neige_dns_cancel(int fd) { cancel_query(fd); }

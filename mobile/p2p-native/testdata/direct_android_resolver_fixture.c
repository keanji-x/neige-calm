#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Only loaded in a host test child. Records the real production wrapper's ABI
 * arguments/ownership; DNS response bytes come from dnsmessage fixtures. */
static int write_fd = -1;
static void trace(const char *event) {
    FILE *file = fopen(getenv("NEIGE_TEST_ANDROID_TRACE"), "a");
    if (!file) abort();
    fprintf(file, "%s\n", event);
    fclose(file);
}
int android_res_nquery(uint64_t network, const char *name, int klass, int type, uint32_t flags) {
    trace("query");
    if (network != 0 || klass != 1 || type != 28 || flags != 0 || strcmp(name, "example.com") != 0) abort();
    const char *mode = getenv("NEIGE_TEST_ANDROID_MODE");
    if (strcmp(mode, "start-error") == 0) return -EIO;
    if (strcmp(mode, "late-start") == 0) usleep(150000);
    int fds[2];
    if (pipe(fds) != 0) abort();
    write_fd = fds[1];
    if (strcmp(mode, "timeout") != 0) {
        if (write(write_fd, "x", 1) != 1) abort();
    }
    return fds[0];
}
#ifndef OMIT_RESULT
int android_res_nresult(int fd, int *rcode, uint8_t *answer, size_t length) {
    trace("result");
    if (!(fcntl(fd, F_GETFL) & O_NONBLOCK)) abort();
    close(fd); close(write_fd); write_fd = -1;
    const char *mode = getenv("NEIGE_TEST_ANDROID_MODE");
    if (strcmp(mode, "result-error") == 0) return -EIO;
    *rcode = strcmp(mode, "servfail") == 0 ? 2 : 0;
    FILE *file = fopen(getenv("NEIGE_TEST_ANDROID_PACKET"), "rb");
    if (!file) abort();
    size_t size = fread(answer, 1, length, file);
    fclose(file);
    return size;
}
#endif
void android_res_cancel(int fd) {
    trace("cancel");
    if (close(fd) != 0) abort();
    if (write_fd >= 0) { close(write_fd); write_fd = -1; }
}

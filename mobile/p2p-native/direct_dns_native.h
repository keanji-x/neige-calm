#ifndef NEIGE_DIRECT_DNS_NATIVE_H
#define NEIGE_DIRECT_DNS_NATIVE_H
#include <stddef.h>
#include <stdint.h>
int neige_dns_query(const char *host, int type);
int neige_dns_result(int fd, int *rcode, uint8_t *answer, size_t length);
void neige_dns_cancel(int fd);
#endif

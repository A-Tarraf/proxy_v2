#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

void *metric_proxy_init(void);

int metric_proxy_release(void *pclient);

void *metric_proxy_new_counter(void *pclient, const char *name, const char *doc);

#ifdef __cplusplus
} // extern "C"
#endif // __cplusplus

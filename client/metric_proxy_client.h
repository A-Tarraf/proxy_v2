#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct MetricProxyClient MetricProxyClient;

typedef struct MetricProxyValue MetricProxyValue;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

struct MetricProxyClient *metric_proxy_init(void);

int metric_proxy_release(struct MetricProxyClient *pclient);

struct MetricProxyValue *metric_proxy_counter_new(struct MetricProxyClient *pclient,
                                                  const char *name,
                                                  const char *doc);

int metric_proxy_counter_inc(struct MetricProxyValue *pcounter, double value);

struct MetricProxyValue *metric_proxy_gauge_new(struct MetricProxyClient *pclient,
                                                const char *name,
                                                const char *doc);

int metric_proxy_gauge_set(struct MetricProxyValue *pcounter, double value);

#ifdef __cplusplus
} // extern "C"
#endif // __cplusplus

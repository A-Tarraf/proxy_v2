#include <metric_proxy_client.h>
#include <stdio.h>


int main(int argc, char ** argv)
{
	struct MetricProxyClient * pclient = metric_proxy_init();

	printf("Client %p\n", pclient);

	struct MetricProxyClientCounter * pcounter = metric_proxy_counter_new(pclient, "key", "test key");

	printf("Counter %p\n", pcounter);


	while (1) {
		metric_proxy_counter_inc(pcounter, 1);

	}

	metric_proxy_release(pclient);

	return 0;
}
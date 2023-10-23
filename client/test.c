#include <metric_proxy_client.h>
#include <stdio.h>


int main(int argc, char ** argv)
{
	struct MetricProxyClient * pclient = metric_proxy_init();

	printf("Client %p\n", pclient);


	struct MetricProxyClientCounter * scounter = metric_proxy_counter_new(pclient, "starts", "number of starts");

	metric_proxy_counter_inc(scounter, 1);


	struct MetricProxyClientCounter * pcounter = metric_proxy_counter_new(pclient, "key", "test key");

	printf("Counter %p\n", pcounter);



	metric_proxy_release(pclient);

	return 0;
}
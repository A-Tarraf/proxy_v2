#include <metric_proxy_client.h>
#include <stdio.h>


int main(int argc, char ** argv)
{
	struct MetricProxyClient * pclient = metric_proxy_init();


	struct MetricProxyClientCounter * scounter = metric_proxy_counter_new(pclient, "starts", "number of starts");

	metric_proxy_counter_inc(scounter, 1);


	struct MetricProxyClientCounter * pcounter = metric_proxy_counter_new(pclient, "key", "test key");

	metric_proxy_release(pclient);

	return 0;
}
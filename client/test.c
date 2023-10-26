#include <metric_proxy_client.h>
#include <stdio.h>
#include <unistd.h>

int main(int argc, char ** argv)
{
	struct MetricProxyClient * pclient = metric_proxy_init();


	struct MetricProxyValue * scounter = metric_proxy_counter_new(pclient, "starts", "number of starts");

	metric_proxy_counter_inc(scounter, 1);


	struct MetricProxyValue * pcounter = metric_proxy_counter_new(pclient, "key", "test key");

	int cnt = 0;

		struct MetricProxyValue * pgauge = metric_proxy_gauge_new(pclient, "loop_counter", "counter of my while loop");


	while(cnt < 100)
	{
		metric_proxy_counter_inc(pcounter, 1);
		cnt++;
		metric_proxy_gauge_set(pgauge, cnt);
		sleep(1);
	}

	metric_proxy_release(pclient);

	return 0;
}
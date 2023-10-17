#include <metric_proxy_client.h>
#include <stdio.h>


int main(int argc, char ** argv)
{
	void * pclient = metric_proxy_init();

	printf("Client %p\n", pclient);

	void * pcounter = metric_proxy_new_counter(pclient, "key", "test key");

	printf("Counter %p\n", pcounter);

	metric_proxy_release(pclient);

	return 0;
}
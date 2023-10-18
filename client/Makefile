CC=gcc

all: test

metric_proxy_client.h: 
	cbindgen .. -o metric_proxy_client.h -l c --cpp-compat


test: ./test.c metric_proxy_client.h
	$(CC) -L$(PWD)/../target/debug/ ./test.c -I. -o $@ -lproxyclient -Wl,-rpath=$(PWD)/../target/debug/

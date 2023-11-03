## Listing Jobs

Job management offers the following endpoints:

- A list of current jobs and their metadata at [http://127.0.0.1:1337/job/list](http://127.0.0.1:1337/joblist)

		[
			{
				"jobid": "main",
				"command": "Sum of all Jobs",
				"size": 0,
				"nodelist": "",
				"partition": "",
				"cluster": "",
				"run_dir": "",
				"start_time": 0,
				"end_time": 0
			},
			{
				"jobid": "Node: deneb",
				"command": "Sum of all Jobs running on deneb",
				"size": 0,
				"nodelist": "deneb",
				"partition": "",
				"cluster": "",
				"run_dir": "",
				"start_time": 0,
				"end_time": 0
			}
		]

- A global view of all jobs all at once [http://localhost:1337/job](http://localhost:1337/job) it includes **all** the data (metadata and counters)


		[
		{
			"desc": {
				"jobid": "main",
				"command": "Sum of all Jobs",
				"size": 0,
				"nodelist": "",
				"partition": "",
				"cluster": "",
				"run_dir": "",
				"start_time": 0,
				"end_time": 0
			},
			"counters": [
				{
					"name": "proxy_cpu_total",
					"doc": "Number of tracked CPUs by individual proxies",
					"ctype": {
						"Gauge": {
							"min": 8,
							"max": 8,
							"hits": 1,
							"total": 8
						}
					}
				},
				...
			]
		},
		{
			"desc": {
				"jobid": "Node: deneb",
				"command": "Sum of all Jobs running on deneb",
				"size": 0,
				"nodelist": "deneb",
				"partition": "",
				"cluster": "",
				"run_dir": "",
				"start_time": 0,
				"end_time": 0
			},
			"counters": [
				{
					"name": "proxy_network_receive_packets_total{interface=\"docker0\"}",
					"doc": "Total number of packets received on the given device",
					"ctype": {
						"Counter": {
							"value": 0
						}
					}
				},
				...
			]
		}
		]

- A JSON export of jobs [http://localhost:1337/job/?job=main](http://localhost:1337/job/?job=main) it filters only the job of interest instead of returning the full array of jobs. It extracts the jobfrom the array given by [http://localhost:1337/job](http://localhost:1337/job) and has the same structure.


- A prometheus export for each job [http://localhost:1337/metrics/?job=testjob](http://localhost:1337/metrics/?job=testjob). Note [http://localhost:1337/metrics](http://localhost:1337/metrics) is the export of the main job and thus equivalent to [http://localhost:1337/metrics/?job=main](http://localhost:1337/metrics/?job=main)


## Managing Alarms

See the [Alarm Example GUI](/alarms.html) for reference.

We propose the following endpoints for alarms:

- [http://127.0.0.1:1337/alarms](http://127.0.0.1:1337/alarms) : list current raised alarms


			{
			"main": [
				{
				"name": "My Alarm",
				"metric": "proxy_cpu_load_average_percent",
				"operator": {
						"More": 33
				},
				"current": 51.56660318374634,
				"active": true,
				"pretty": "My Alarm : proxy_cpu_load_average_percent (Average load on all the CPUs) = 51.56660318374634 (Min: 51.56660318374634, Max : 51.56660318374634, Hits: 1, Total : 51.56660318374634) GAUGE > 33"
				}
			]
			}

- [http://127.0.0.1:1337/alarms/list](http://127.0.0.1:1337/alarms/list) : list all registered alarms

		{
		"main": [
			{
			"name": "My Other Alarm",
			"metric": "proxy_cpu_load_average_percent",
			"operator": {
					"Less": 33
			},
			"current": 6.246700286865234,
			"active": true,
			"pretty": "My Other Alarm : proxy_cpu_load_average_percent (Average load on all the CPUs) = 6.246700286865234 (Min: 6.246700286865234, Max : 6.246700286865234, Hits: 1, Total : 6.246700286865234) GAUGE < 33"
			},
			{
			"name": "My Alarm",
			"metric": "proxy_cpu_load_average_percent",
			"operator": {
					"More": 33
			},
			"current": 6.246700286865234,
			"active": false,
			"pretty": "My Alarm : proxy_cpu_load_average_percent (Average load on all the CPUs) = 6.246700286865234 (Min: 6.246700286865234, Max : 6.246700286865234, Hits: 1, Total : 6.246700286865234) GAUGE > 33"
			}
		]
		}


- [http://127.0.0.1:1337/alarms/add](http://127.0.0.1:1337/alarms/add) : add a new alarm


	Takes a JSON object

		{
			"name": "My Alarm",
			"target": "main",
			"metric": "proxy_cpu_load_average_percent",
			"operation": ">",
			"value": 33
		}


	You can make it with curl:

		curl -s http://localhost:1337/alarms/add\
		-H "Content-Type: application/json" \
		-d '{ "name": "My Alarm", "target": "main", "metric": "proxy_cpu_load_average_percent", "operation": ">", "value": 33 }'
	


	Operation can be "<" ">" and "=" to w.r.t. value.


- [http://127.0.0.1:1337/alarms/del](http://127.0.0.1:1337/alarms/del) : delete an existing alarm


    - Using the GET protocol:
        - **targetjob** : name of the job
        - **name** : name of the alarm

        Example with Curl

			# Be careful with the & in bash/sh
			curl "http://localhost:1337/alarms/del?targetjob=main&name=My%20Alarm"


    - Using the POST protocol:

        Send the following JSON:
       
			{
					"target": "main",
					"name": "My Alarm",
			}


        Example with Curl:

			curl -s http://localhost:1337/alarms/del \
			-H "Content-Type: application/json" \
			-d '{"target": "main", "name": "My Alarm"}'


## Adding New Scrapes using /join

It is possible to request a proxy to scrape a given target. Currently the following targets are supported:

- Another proxy meaning you may pass the url to another proxy to have it collected by the current proxt
- A prometheus exporter, meaning the `/metric` endpoint will be harvested, currently only counters and gauges are handled. In the case of prometheus scrapes, they are aggregated only in "main" and inside the "node" specific job.

Only the GET requests are supported using the `to` argument, for example:

[http://localhost:1337/join?to=localhost:9100](http://localhost:1337/join?to=localhost:9100) will add the [node exporter](https://github.com/prometheus/node_exporter) running on localhost (classically on [http://localhost:9100](http://localhost:9100)) and the proxy is able to scrape such metrics.


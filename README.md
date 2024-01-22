# Metric Proxy

Metric Proxy is a Prometheus Aggregating Push Gateway designed for use in the ADMIRE project. It acts as an instrumentation proxy, collecting and aggregating metrics from various sources to provide a centralized view for monitoring and analysis

## Install Rust

:::info
Do this step only the first time you install
:::

Simply run:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#Answer "1" proceed to install to the question (or customize accordingly)
```

## Compile Proxy V2

### Dependencies

You need:
- a python in env (python / python3)
- mpicc in env (any MPI but same as your app)
- Rust installed

### Get Sources

```bash
git clone https://github.com/besnardjb/proxy_v2.git
```

### Compile

Simply enter inside the source directory `cd ./proxy_v2` and run `./install [PREFIX]`.

:::info
You may need to tweak the OpenSSL directory using `export OPENSSL_DIR="/usr/local/ssl` if there is an issue compiling the OpenSSL crate.
:::

### TL;DR

```sh
git clone https://github.com/besnardjb/proxy_v2.git
cd proxy_v2
# Install the proxy (requires mpicc in the path)
./install.sh $HOME/metric-proxy
# Add the prefix to your path
export PATH=$HOME/metric-proxy/bin:$PATH
# Run the server (and keep it running)
proxy-v2
# Run the client in another shell1
proxy_run -j testls -- ls -R /
```

Then open http://localhost:1337

## Proxy Design

The new proxy (also called V2) shares several design approaches with the first iteration of its kind with the following changes:

- The proxy now includes an internal web-gui (by default at http://localhost:1337)
- The proxy is capable of creating a reduction tree for metrics
- Data are tracked per-job basis in real-time
- The proxy now includes the capability of tracking node state (node-level metrics)


## Running the Proxy

### Running Manually

The `proxy_run` command is to be used to run instrumented programs. This program supposed that a metric proxy is running on each of the target nodes.

Help is the following:
```
Usage: proxy_run [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...  A command to run (passed after --)

Options:
  -l, --listexporters            List detected exporters
  -e, --exporter <EXPORTER>      List of exporters to activate
  -j, --jobid <JOBID>            Optionnal JOBID (MPI/SLURM may generate one automatically)
  -u, --unixsocket <UNIXSOCKET>  Optionnal path to proxy UNIX Socket
  -h, --help                     Print help
  -V, --version                  Print version
``` 

A basic usage is:

```sh
# MPIRUN
mpirun -np 8 proxy_run -- ./myprogram -XXX
# srun
srun -n 8 proxy_run -- ./myprogram -XXX
```


The metric proxy now supports jobs, you can force a job using the `-j` flag or resort to the slurm / mpirun default. Note, a job may have no jobid in some circumstances and thus not appear as a job if you open the job interface in http://localhost:1337/jobs.html.

For example if you do:

```
proxy_run -j testjob -- yes
```

now have a `testjob` line:

![](https://france.paratools.com/hedgedoc/uploads/7c4884e1-b9af-4d21-ab11-01656c633c64.png)


You can then click on `testjob` to see the metrics flowing:

![](https://france.paratools.com/hedgedoc/uploads/5af635d1-1335-451c-9089-5b8373e8114f.png)



:::info
Both job metrics and node level metrics are collated in these metrics to ease the blaming of node performance on job performance. Note that node level metrics are transposed indiferently of a possible partial allocation.
:::

### Note on Job-Related Data Endpoints

Unlike previous proxy which exposed only Prometheus endpoint, we have reworked our approach to expose more structured data including for each job:

Job management offers the following endpoints:

- A list of current jobs and their metadata at [http://127.0.0.1:1337/job/list](http://127.0.0.1:1337/joblist)

```json
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
```

- A global view of all jobs all at once [http://localhost:1337/job](http://localhost:1337/job) it includes **all** the data (metadata and counters)

```json
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
```

- A JSON export of jobs [http://localhost:1337/job/?job=main](http://localhost:1337/job/?job=main) it filters only the job of interest instead of returning the full array of jobs. It extracts the jobfrom the array given by [http://localhost:1337/job](http://localhost:1337/job) and has the same structure.


- A prometheus export for each job [http://localhost:1337/metrics/?job=testjob](http://localhost:1337/metrics/?job=testjob). Note [http://localhost:1337/metrics](http://localhost:1337/metrics) is the export of the main job and thus equivalent to [http://localhost:1337/metrics/?job=main](http://localhost:1337/metrics/?job=main)




## Setting Alarms

You may set alarms to track values see the example GUI at http://127.0.0.1:1337/alarms.html.

We propose the following endpoints:

- http://127.0.0.1:1337/alarms : list current raised alarms

```json
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
```

- http://127.0.0.1:1337/alarms/list : list all registered alarms

```json
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
```

- http://127.0.0.1:1337/alarms/add : add a new alarm

:::info
Takes a JSON object
```json
{
    "name": "My Alarm",
    "target": "main",
    "metric": "proxy_cpu_load_average_percent",
    "operation": ">",
    "value": 33
}
```

You can make it with curl:

```bash
curl -s http://localhost:1337/alarms/add\
  -H "Content-Type: application/json" \
  -d '{ "name": "My Alarm", "target": "main", "metric": "proxy_cpu_load_average_percent", "operation": ">", "value": 33 }'
``` 


Operation can be "<" ">" and "=" to w.r.t. value.

:::
- http://127.0.0.1:1337/alarms/del : delete an existing alarm

:::info

- Using the GET protocol:
    - **targetjob** : name of the job
    - **name** : name of the alarm

    Example with Curl
    ```sh
    # Be careful with the & in bash/sh
    curl "http://localhost:1337/alarms/del?targetjob=main&name=My%20Alarm"
    ```

- Using the POST protocol:

    Send the following JSON:
    ```json
    {
        "target": "main",
        "name": "My Alarm",
    }
    ```

    Example with Curl:
    ```bash
    curl -s http://localhost:1337/alarms/del \
      -H "Content-Type: application/json" \
      -d '{"target": "main", "name": "My Alarm"}'
    ``` 


:::

## Scanning Finished Jobs (Profiles)

As exposed in the [example GUI](/profiles.html), for manipulating profiles (final snapshot of jobs) the folowing JSON endpoints are provided:

- [http://127.0.0.1:1337/profiles](http://127.0.0.1:1337/profiles) a list of profiles on the system, data layout is a job description as shown in [http://127.0.0.1:1337/joblist](http://127.0.0.1:1337/joblist)
- [http://127.0.0.1:1337/percmd](http://127.0.0.1:1337/percmd) a list of profiles gathered by launch command to ease procesing by command

```json
{
    "./command_a ": [
            {
                "jobid": "test2",
                "command": "./command_a ",
                "size": 1,
                "nodelist": "",
                "partition": "",
                "cluster": "",
                "run_dir": "/XXX/proxy_v2/client",
                "start_time": 1699020416,
                "end_time": 1699020421
            }
        ],
    "./command_b ": [
            {
                "jobid": "test1",
                "command": "./command_b ",
                "size": 1,
                "nodelist": "",
                "partition": "",
                "cluster": "",
                "run_dir": "/XXX/proxy_v2/client",
                "start_time": 1699020317,
                "end_time": 1699020398
            }
        ]
}
```


[http://127.0.0.1:1337/get?jobid=XXX](http://127.0.0.1:1337/get?jobid=XXX) allows to get a given profile, layout is identical to a job JSON snapshot as exposed in [http://localhost:1337/job/?job=main](http://localhost:1337/job/?job=main).

## Adding New Scrapes using /join

It is possible to request a proxy to scrape a given target. Currently the following targets are supported:

- Another proxy meaning you may pass the url to another proxy to have it collected by the current proxt
- A prometheus exporter, meaning the `/metric` endpoint will be harvested, currently only counters and gauges are handled. In the case of prometheus scrapes, they are aggregated only in "main" and inside the "node" specific job.

Only the GET requests are supported using the `to` argument, for example:

[http://localhost:1337/join?to=localhost:9100](http://localhost:1337/join?to=localhost:9100) will add the [node exporter](https://github.com/prometheus/node_exporter) running on localhost (classically on [http://localhost:9100](http://localhost:9100)) and the proxy is able to scrape such metrics.

You can get the list of current scrapes at [http://localhost:1337/join/list](http://localhost:1337/join/list)

It consists in such JSON:

```json
[
  {
    "target_url": "http://localhost:9100/metrics",
    "ttype": "Prometheus",
    "period": 5,
    "last_scrape": 1699010032
  },
  {
    "target_url": "/system",
    "ttype": "System",
    "period": 5,
    "last_scrape": 1699010032
  }
]
```

## Running at UNITO Cluster

:::warning
To install on UNITO you need mpicc in the path and it is only the case on compute nodes you may run install.sh doing:


```bash
# Note on UNITO to install the instrumentation for MPI
# you need mpicc in the path the simplest way
# is to run the install on one of the compute
# nodes by doing:
srun -N1 ./install.sh $HOME/metric-proxy
```

Make sure you have `Sucessfully deployed` written as last line of the output when deploying, if not there was a previous issue.

:::


To run at unito you first need to allocate some nodes, lets say to start 3 nodes. Note this process is only due to the fact that the ADMIRE infrastructure has to be deployed manually, these services should stay on in a final system.

```bash
$ salloc -N 3 -p broadwell
    salloc: Granted job allocation 24721
    salloc: Nodes broadwell-[063-065] are ready for job
# From now on any srun in this shell is relative to these 3 nodes (for now keep this shell open entering no commands)
``` 

Now to simplify things, we will run the root proxy on the frontend, if you have sourced the proxy in your path:

```bash
export PATH=$HOME/metric-proxy/bin:$PATH
```

You may simply run:

```bash
# In another shell note if multiple proxies run on the frontend their 1337 port may clash you may change it with -p 1999 (or any other to solve it)
proxy_v2 -p 1337
``` 

Now open one last shell to run the metric proxies on the target nodes (we do not do it using srun as we do not want to book the nodes in slurm)

```bash
# Take the nodelist outputed by the salloc to pass to clush
# We set -i not to reduce profiles in non-root proxes
# We set -r to request the proxies to "pivot" on the root to build a reduction tree
# If you do not have clush just do "pip install --user ClusterShell"
# Run from proxy source root
# Make sure to match the 1337 with the port of your root proxy
clush -w broadwell-[063-065] $HOME/metric-proxy/bin/proxy_v2 -i -r http://c3sfr1:1337
```

:::warning
You need to disable strict host checking to allow clush to connect transparently to all node, run once:

```sh
cat << . >> ~/.ssh/config
Host broadwell-*
    StrictHostKeyChecking no
.
```

:::

:::info

To have SSH opening a tunnel to your proxy, you can do:

```bash
# With 1337 the port of the proxy running on the frontend
ssh -L1337:localhost:1337 [YOUR login]@c3sfr1.di.unito.it
```
As we export the port 1337 on your local machine (the first 1337), all URLs related to localhost will in fact go to the target unito machine.

:::


Now you can check the topology of your proxies to see if they found each other at http://127.0.0.1:1337/topology.html


![](https://france.paratools.com/hedgedoc/uploads/872ca83d-d487-44f8-a91f-0bffd72c47cc.png)

:::success
This represents your proxies, each of them run the same webserver as shown on the root proxy. Here they organize themselves in a binary tree.
:::

In this configuration you have 4 jobs:

- One job for each node prefixed with `Node :`
- A main job which sums them all

![](https://france.paratools.com/hedgedoc/uploads/a80d434c-7361-41e5-aac2-4a6fc05fd0ca.png)

Now if you take your first shell, the one of the salloc you can run a job inside the distributed tree, lets start simply with our test program (in the `./client`  directory):

```bash
# Make sure to have the launch command in the path
export PATH=$HOME/metric-proxy/bin:$PATH
# On unito we have 3 nodes of 36 cores on broadwell
srun -n 108 proxy_run -- ./test
```

And then the given slurm job id pops in the joblist:

![](https://france.paratools.com/hedgedoc/uploads/ba9ae5b5-5771-4e87-bb41-67943ea4f85a.png)

:::warning
When the job finishes it state is wiped out of the proxy and a profile is generated in the `~/.proxyprofiles/profiles` directory. This profile contains the sum of all the nodes (the profile of the job). It is exposed in the JSON format as shown on the http://127.0.0.1:1337/job?job=XXX endpoint.
:::


## Acknowledgments

This project has received funding from the European Union’s Horizon 2020 JTI-EuroHPC research and innovation programme with grant Agreement number: 956748



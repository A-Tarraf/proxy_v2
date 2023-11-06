#include <mpi.h>

#include <string.h>
#include <ctype.h>
#include <stdlib.h>
#include <stdio.h>
#include <pthread.h>
#include <sys/time.h>
#include <stdio.h>
#include <unistd.h>

#include "cycle.h"


#include "metric_proxy_client.h"

static struct MetricProxyClient * __client = NULL;

static ticks ticks_per_second = 0;

static inline ticks get_ticks_per_second()
{
   return ticks_per_second;
}

void calibrate_ticks()
{
   char * forced_ticks = getenv("FORCED_TICKS");

   if(forced_ticks)
   {
      char * end = NULL;
      ticks_per_second = strtol(forced_ticks, &end, 10);
   }
   else
   {
      struct timeval tv_start, tv_end;

      fprintf(stderr, "Calibrating timer ...\n");

      ticks start = getticks();
      gettimeofday(&tv_start, NULL);

      sleep(1);

      ticks end = getticks();
      gettimeofday(&tv_end, NULL);

      ticks_per_second = (end - start) / (((double)tv_end.tv_sec + (double)tv_end.tv_usec *1e-6) - ((double)tv_start.tv_sec + (double)tv_start.tv_usec *1e-6));
   }
}


static inline char * tolower_in_buff(const char * src, char * buff, int buff_size)
{
  int i;
  int len = strlen(src);

  for(i = 0 ; (i < len) && (i < buff_size); i++)
  {
    buff[i] = tolower(src[i]);
  }

  buff[len] = '\0';

  return buff;
}

typedef enum
{
  TAU_MPI_TIME,
  TAU_MPI_HITS,
  TAU_MPI_SIZE,
  TAU_MPI_SIZE_IN,
  TAU_MPI_SIZE_OUT,
{{forallfn foo}}
  TAU_METRIC_{{foo}}_HITS,
  TAU_METRIC_{{foo}}_TIME,
  /* May not be relevant for all */
  TAU_METRIC_{{foo}}_SIZE,
  TAU_METRIC_{{foo}}_SIZE_IN,
  TAU_METRIC_{{foo}}_SIZE_OUT,


{{endforallfn}}
  TAU_METRICS_COUNT
}mpi_wrapper_metrics_t;

static struct MetricProxyValue * __counters[TAU_METRICS_COUNT] = { 0 };
pthread_spinlock_t __counters_creation_lock;

static inline void __define_counter(mpi_wrapper_metrics_t slot,
                                    const char * fn_name,
                                    const char * counter_suffix,
                                    const char * doc)
{
  char lower_fn_name[128];
  char tmp_buff[1024];
  char doc_tmp_buff[1024];

  tolower_in_buff(fn_name, lower_fn_name, 128);

  snprintf(tmp_buff, 1024, "%s_total{function=\"%s\"}",counter_suffix, lower_fn_name);
  snprintf(doc_tmp_buff, 1024, "%s for %s", doc, fn_name);
  __counters[slot] = metric_proxy_counter_new(__client, tmp_buff, doc_tmp_buff);
}



/* This is the initialization function */
void mpi_wrapper_initialize(void)
{
  __client = metric_proxy_init();
  calibrate_ticks();
  pthread_spin_init(&__counters_creation_lock, 0);

  char * fn_name = NULL;
  char lower_fn_name[128];
  char tmp_buff[512];
  char doc_tmp_buff[512];

  int cnt = 0;

  int i;

  for(i = 0 ; i < TAU_METRICS_COUNT ; i++)
  {
    __counters[i] = NULL;
  }


  __counters[TAU_MPI_TIME] = metric_proxy_counter_new(__client, "mpi_total{metric=\"time\"}", "Aggregated MPI metrics");
  __counters[TAU_MPI_HITS] = metric_proxy_counter_new(__client, "mpi_total{metric=\"hits\"}", "Aggregated MPI metrics");
  __counters[TAU_MPI_SIZE] = metric_proxy_counter_new(__client, "mpi_total{metric=\"size\"}", "Aggregated MPI metrics");
  __counters[TAU_MPI_SIZE_IN] = metric_proxy_counter_new(__client, "mpi_total{metric=\"size_in\"}", "Aggregated MPI metrics");
  __counters[TAU_MPI_SIZE_OUT] = metric_proxy_counter_new(__client, "mpi_total_size{metric=\"size_out\"}", "Aggregated MPI metrics");


{{forallfn foo}}
  fn_name = "{{foo}}";
   tolower_in_buff(fn_name, lower_fn_name, 1024);

  /* Register counters */
  __define_counter(TAU_METRIC_{{foo}}_HITS, fn_name, "hits", "Number of function calls");
  cnt++;

  __define_counter(TAU_METRIC_{{foo}}_TIME, fn_name, "time", "Total seconds spent");
  cnt++;

{{endforallfn}}

}


static inline void __ensure_size_counter_is_available(char * func_name,
                                                      mpi_wrapper_metrics_t size_slot,
                                                      mpi_wrapper_metrics_t size_in_slot,
                                                      mpi_wrapper_metrics_t size_out_slot)
{
  if(!__counters[size_slot])
  {

    pthread_spin_lock(&__counters_creation_lock);

    if(!__counters[size_slot])
    {
      /* Create the counter for the size type */
      __define_counter(size_slot, func_name, "size", "Total size (IN + OUT)");
    }

    if(!__counters[size_in_slot])
    {
      /* Create the counter for the size type */
        __define_counter(size_in_slot, func_name, "size_in", "Total size (IN)");
    }

    if(!__counters[size_out_slot])
    {
      /* Create the counter for the size type */
        __define_counter(size_out_slot, func_name, "size_out", "Total size (OUT)");
    }

    pthread_spin_unlock(&__counters_creation_lock);

  }
}

#define CALL_START(hits_counter)   metric_proxy_counter_inc(__counters[ hits_counter ], 1); \
                          metric_proxy_counter_inc(__counters[TAU_MPI_HITS], 1);\
                          ticks time_at_start = getticks();

#define CALL_END(time_counter)   ticks time_at_end = getticks(); \
                        double duration = (double)(time_at_end - time_at_start)/get_ticks_per_second(); \
                        metric_proxy_counter_inc(__counters[time_counter], duration);\
                        metric_proxy_counter_inc(__counters[TAU_MPI_TIME], duration);


#define CALL_SIZE(func, s, sin, sout) if(_size != 0) \
                                      {\
                                        __ensure_size_counter_is_available(func, s, sin, sout);\
                                        metric_proxy_counter_inc(__counters[TAU_MPI_SIZE], _size);\
                                        metric_proxy_counter_inc(__counters[TAU_MPI_SIZE_IN], _size_in);\
                                        metric_proxy_counter_inc(__counters[TAU_MPI_SIZE_OUT], _size_out);\
                                        metric_proxy_counter_inc(__counters[s], _size);\
                                        metric_proxy_counter_inc(__counters[sin], _size_in);\
                                        metric_proxy_counter_inc(__counters[sout], _size_out);\
                                      }


/* Handle INIT */
{{fn foo MPI_Init MPI_Init_thread}}
  mpi_wrapper_initialize();

  CALL_START(TAU_METRIC_{{foo}}_HITS)

  {{callfn}}

  CALL_END(TAU_METRIC_{{foo}}_TIME)

{{endfn}}


/* Handle Finalize */
{{fn foo MPI_Finalize}}
  CALL_START(TAU_METRIC_{{foo}}_HITS)

  {{callfn}}

  CALL_END(TAU_METRIC_{{foo}}_TIME)

  metric_proxy_release(__client);
{{endfn}}



/* All functions with no size */

{{fnall foo MPI_Init MPI_Init_thread MPI_Finalize}}

  CALL_START(TAU_METRIC_{{foo}}_HITS)

  {{callfn}}

  CALL_END(TAU_METRIC_{{foo}}_TIME)

  {{size}}

  CALL_SIZE("{{foo}}", TAU_METRIC_{{foo}}_SIZE, TAU_METRIC_{{foo}}_SIZE_IN, TAU_METRIC_{{foo}}_SIZE_OUT)

{{endfnall}}

#ifndef SYSROOT_SCHED_H_
#define SYSROOT_SCHED_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <features.h>

#define __NEED_struct_timespec
#define __NEED_pid_t
#define __NEED_time_t

#ifdef _GNU_SOURCE
#define __NEED_size_t
#endif

#include <bits/alltypes.h>

struct sched_param {
  int sched_priority;
  int sched_ss_low_priority;
  struct timespec sched_ss_repl_period;
  struct timespec sched_ss_init_budget;
  int sched_ss_max_repl;
};

int sched_get_priority_max(int);
int sched_get_priority_min(int);
int sched_getparam(pid_t, struct sched_param*);
int sched_getscheduler(pid_t);
int sched_rr_get_interval(pid_t, struct timespec*);
int sched_setparam(pid_t, const struct sched_param*);
int sched_setscheduler(pid_t, int, const struct sched_param*);
int sched_yield(void);

#define SCHED_OTHER 0
#define SCHED_FIFO 1
#define SCHED_RR 2
#define SCHED_BATCH 3
#define SCHED_IDLE 5
#define SCHED_DEADLINE 6
#define SCHED_RESET_ON_FORK 0x40000000

#ifdef _GNU_SOURCE
void* memcpy(void* __restrict, const void* __restrict, size_t);
int memcmp(const void*, const void*, size_t);
void* calloc(size_t, size_t) __nothrow_fn;
void free(void*) __nothrow_fn;

typedef struct cpu_set_t {
  unsigned long __bits[128 / sizeof(long)];
} cpu_set_t;
int __sched_cpucount(size_t, const cpu_set_t*);
int sched_getcpu(void);
int sched_getaffinity(pid_t, size_t, cpu_set_t*);
int sched_setaffinity(pid_t, size_t, const cpu_set_t*);

#define __CPU_op_S(i, size, set, op) \
  ((i) / 8U >= (size)                \
       ? 0                           \
       : ((set)->__bits[(i) / 8 / sizeof(long)] op(1UL << ((i) % (8 * sizeof(long))))))

#define CPU_SET_S(i, size, set) __CPU_op_S(i, size, set, |=)
#define CPU_CLR_S(i, size, set) __CPU_op_S(i, size, set, &= ~)
#define CPU_ISSET_S(i, size, set) __CPU_op_S(i, size, set, &)

#define __CPU_op_func_S(func, op)                                                                  \
  static __inline void __CPU_##func##_S(size_t __size, cpu_set_t* __dest, const cpu_set_t* __src1, \
                                        const cpu_set_t* __src2) {                                 \
    size_t __i;                                                                                    \
    for (__i = 0; __i < __size / sizeof(long); __i++)                                              \
      __dest->__bits[__i] = __src1->__bits[__i] op __src2->__bits[__i];                            \
  }

__CPU_op_func_S(AND, &) __CPU_op_func_S(OR, |) __CPU_op_func_S(XOR, ^)

#define CPU_AND_S(a, b, c, d) __CPU_AND_S(a, b, c, d)
#define CPU_OR_S(a, b, c, d) __CPU_OR_S(a, b, c, d)
#define CPU_XOR_S(a, b, c, d) __CPU_XOR_S(a, b, c, d)

#define CPU_COUNT_S(size, set) __sched_cpucount(size, set)
#define CPU_ZERO_S(size, set) memset(set, 0, size)
#define CPU_EQUAL_S(size, set1, set2) (!memcmp(set1, set2, size))

#define CPU_ALLOC_SIZE(n)                     \
  (sizeof(long) * ((n) / (8 * sizeof(long)) + \
                   ((n) % (8 * sizeof(long)) + 8 * sizeof(long) - 1) / (8 * sizeof(long))))
#define CPU_ALLOC(n) ((cpu_set_t*)calloc(1, CPU_ALLOC_SIZE(n)))
#define CPU_FREE(set) free(set)

#define CPU_SETSIZE 128

#define CPU_SET(i, set) CPU_SET_S(i, sizeof(cpu_set_t), set)
#define CPU_CLR(i, set) CPU_CLR_S(i, sizeof(cpu_set_t), set)
#define CPU_ISSET(i, set) CPU_ISSET_S(i, sizeof(cpu_set_t), set)
#define CPU_AND(d, s1, s2) CPU_AND_S(sizeof(cpu_set_t), d, s1, s2)
#define CPU_OR(d, s1, s2) CPU_OR_S(sizeof(cpu_set_t), d, s1, s2)
#define CPU_XOR(d, s1, s2) CPU_XOR_S(sizeof(cpu_set_t), d, s1, s2)
#define CPU_COUNT(set) CPU_COUNT_S(sizeof(cpu_set_t), set)
#define CPU_ZERO(set) CPU_ZERO_S(sizeof(cpu_set_t), set)
#define CPU_EQUAL(s1, s2) CPU_EQUAL_S(sizeof(cpu_set_t), s1, s2)

#endif

#ifdef __cplusplus
}
#endif

#endif  // SYSROOT_SCHED_H_

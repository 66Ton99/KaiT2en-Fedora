/* SPDX-License-Identifier: GPL-2.0-only */
/* Host-only clock and MMIO stand-ins for compiling the production mailbox. */
#ifndef T2SEP_MAILBOX_TEST_SHIM_H
#define T2SEP_MAILBOX_TEST_SHIM_H
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
typedef uint32_t u32;
typedef int64_t ktime_t;
#define __iomem
#define BIT(n) (1U << (n))
#define ENODATA 61
#define ETIMEDOUT 110
ktime_t ktime_get(void);
static inline int ktime_compare(ktime_t a, ktime_t b)
{
	return (a > b) - (a < b);
}
static inline ktime_t ktime_add_ms(ktime_t base, uint64_t ms)
{
	return base + (ktime_t)(ms * 1000000);
}
u32 readl(const void *address);
void writel(u32 value, void *address);
void usleep_range(unsigned long minimum, unsigned long maximum);
#endif

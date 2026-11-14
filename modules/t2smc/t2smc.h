/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _T2SMC_H_
#define _T2SMC_H_

#include <linux/types.h>

struct t2smc_device;

int t2smc_commit_bridgeos_boot(struct t2smc_device *t2);

#endif /* _T2SMC_H_ */

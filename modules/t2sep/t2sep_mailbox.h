/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef T2SEP_MAILBOX_H
#define T2SEP_MAILBOX_H

#include <linux/io.h>
#include <linux/ktime.h>
#include <linux/types.h>

#define T2SEP_MAILBOX_MIN_SIZE 0x10000

struct t2sep_message {
	u32 word[4];
};

struct t2sep_mailbox {
	void __iomem *base;
};

void t2sep_mailbox_init(struct t2sep_mailbox *mailbox, void __iomem *base);
u32 t2sep_mailbox_inbox_status(struct t2sep_mailbox *mailbox);
u32 t2sep_mailbox_outbox_status(struct t2sep_mailbox *mailbox);
bool t2sep_mailbox_inbox_empty(struct t2sep_mailbox *mailbox);
bool t2sep_mailbox_outbox_full(struct t2sep_mailbox *mailbox);
int t2sep_mailbox_try_receive(struct t2sep_mailbox *mailbox,
			      struct t2sep_message *message);
/* One absolute monotonic deadline covers send and all reply attempts.
 * Expired operations do not consume or post a mailbox descriptor.
 */
int t2sep_mailbox_receive_until(struct t2sep_mailbox *mailbox,
				struct t2sep_message *message, ktime_t deadline);
int t2sep_mailbox_send_until(struct t2sep_mailbox *mailbox,
			     const struct t2sep_message *message,
			     ktime_t deadline);

#endif

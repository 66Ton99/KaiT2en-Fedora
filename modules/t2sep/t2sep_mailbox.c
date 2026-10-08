// SPDX-License-Identifier: GPL-2.0-only
/* Apple T2 SEP 128-bit mailbox transport. */

#include <linux/bitops.h>
#include <linux/delay.h>
#include <linux/errno.h>

#include "t2sep_mailbox.h"

#define T2SEP_INBOX_STATUS  0x0108
#define T2SEP_OUTBOX_STATUS 0x010c
#define T2SEP_INBOX_DATA    0x0810
#define T2SEP_OUTBOX_DATA   0x0820

/* These bits describe the unavailable state of their respective FIFO. */
#define T2SEP_INBOX_EMPTY   BIT(17)
#define T2SEP_OUTBOX_FULL   BIT(16)

#define T2SEP_POLL_US       100

void t2sep_mailbox_init(struct t2sep_mailbox *mailbox, void __iomem *base)
{
	mailbox->base = base;
}

u32 t2sep_mailbox_inbox_status(struct t2sep_mailbox *mailbox)
{
	return readl(mailbox->base + T2SEP_INBOX_STATUS);
}

u32 t2sep_mailbox_outbox_status(struct t2sep_mailbox *mailbox)
{
	return readl(mailbox->base + T2SEP_OUTBOX_STATUS);
}

bool t2sep_mailbox_inbox_empty(struct t2sep_mailbox *mailbox)
{
	return t2sep_mailbox_inbox_status(mailbox) & T2SEP_INBOX_EMPTY;
}

bool t2sep_mailbox_outbox_full(struct t2sep_mailbox *mailbox)
{
	return t2sep_mailbox_outbox_status(mailbox) & T2SEP_OUTBOX_FULL;
}

int t2sep_mailbox_try_receive(struct t2sep_mailbox *mailbox,
			      struct t2sep_message *message)
{
	if (t2sep_mailbox_inbox_empty(mailbox))
		return -ENODATA;

	message->word[0] = readl(mailbox->base + T2SEP_INBOX_DATA + 0x0);
	message->word[1] = readl(mailbox->base + T2SEP_INBOX_DATA + 0x4);
	message->word[2] = readl(mailbox->base + T2SEP_INBOX_DATA + 0x8);
	/* The final read advances the hardware FIFO and must remain last. */
	message->word[3] = readl(mailbox->base + T2SEP_INBOX_DATA + 0xc);

	return 0;
}

int t2sep_mailbox_receive_until(struct t2sep_mailbox *mailbox,
				struct t2sep_message *message, ktime_t deadline)
{
	for (;;) {
		/* Scheduling can overshoot a requested sleep. Check real elapsed
		 * time before touching the FIFO, including after every wakeup.
		 */
		if (ktime_compare(ktime_get(), deadline) >= 0)
			return -ETIMEDOUT;
		if (!t2sep_mailbox_inbox_empty(mailbox))
			return t2sep_mailbox_try_receive(mailbox, message);
		usleep_range(T2SEP_POLL_US, T2SEP_POLL_US * 2);
	}
}

int t2sep_mailbox_send_until(struct t2sep_mailbox *mailbox,
			     const struct t2sep_message *message,
			     ktime_t deadline)
{
	for (;;) {
		if (ktime_compare(ktime_get(), deadline) >= 0)
			return -ETIMEDOUT;
		if (!t2sep_mailbox_outbox_full(mailbox))
			break;
		usleep_range(T2SEP_POLL_US, T2SEP_POLL_US * 2);
	}

	writel(message->word[0], mailbox->base + T2SEP_OUTBOX_DATA + 0x0);
	writel(message->word[1], mailbox->base + T2SEP_OUTBOX_DATA + 0x4);
	writel(message->word[2], mailbox->base + T2SEP_OUTBOX_DATA + 0x8);
	/* The final write posts the complete message to SEP. */
	writel(message->word[3], mailbox->base + T2SEP_OUTBOX_DATA + 0xc);

	return 0;
}

// SPDX-License-Identifier: GPL-2.0-only
/* Execute production FIFO code against scheduled descriptors and clock stalls. */
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "t2sep_mailbox.h"

#define NS_PER_MS 1000000LL
static unsigned char registers[T2SEP_MAILBOX_MIN_SIZE];
static struct t2sep_mailbox mailbox;
static ktime_t now, sleep_ns, outbox_ready;
static struct t2sep_message replies[3], sent;
static ktime_t reply_ready[3];
static unsigned int reply_count, reply_index, reads, writes, status_reads, sleeps;

ktime_t ktime_get(void)
{
	return now;
}

void usleep_range(unsigned long minimum, unsigned long maximum)
{
	assert(minimum == 100 && maximum == 200);
	/* The scheduler can resume us well after the requested 100--200 us. */
	now += sleep_ns;
	sleeps++;
}

u32 readl(const void *address)
{
	ptrdiff_t offset = (const unsigned char *)address - registers;
	if (offset == 0x108) {
		status_reads++;
		return reply_index >= reply_count || now < reply_ready[reply_index]
			? BIT(17) : 0;
	}
	if (offset == 0x10c) {
		status_reads++;
		return now < outbox_ready ? BIT(16) : 0;
	}
	assert(reply_index < reply_count && now >= reply_ready[reply_index]);
	unsigned int word = reads % 4;
	assert(offset == 0x810 + word * 4);
	u32 value = replies[reply_index].word[word];
	reads++;
	if (word == 3)
		reply_index++;
	return value;
}

void writel(u32 value, void *address)
{
	ptrdiff_t offset = (unsigned char *)address - registers;
	assert(writes < 4 && offset == 0x820 + writes * 4);
	assert(now >= outbox_ready);
	sent.word[writes++] = value;
}

static void reset(void)
{
	now = outbox_ready = 0;
	sleep_ns = 100000;
	reply_count = reply_index = reads = writes = status_reads = sleeps = 0;
	memset(replies, 0, sizeof(replies));
	memset(reply_ready, 0, sizeof(reply_ready));
	memset(&sent, 0, sizeof(sent));
	t2sep_mailbox_init(&mailbox, registers);
}

static void ready_and_fifo_order(void)
{
	struct t2sep_message result, request = { .word = { 1, 2, 3, 0 } };
	reset();
	reply_count = 1;
	replies[0] = request;
	assert(t2sep_mailbox_send_until(&mailbox, &request, NS_PER_MS) == 0);
	assert(writes == 4 && memcmp(&sent, &request, sizeof(sent)) == 0);
	assert(t2sep_mailbox_receive_until(&mailbox, &result, NS_PER_MS) == 0);
	assert(reads == 4 && reply_index == 1);
	assert(memcmp(&result, &request, sizeof(result)) == 0 && sleeps == 0);
}

static void expired_ready_descriptors_are_untouched(void)
{
	struct t2sep_message result = { }, request = { };
	reset();
	reply_count = 1;
	assert(t2sep_mailbox_send_until(&mailbox, &request, now) == -ETIMEDOUT);
	assert(t2sep_mailbox_receive_until(&mailbox, &result, now) == -ETIMEDOUT);
	now = 10 * NS_PER_MS;
	assert(t2sep_mailbox_receive_until(&mailbox, &result, now - 1) == -ETIMEDOUT);
	assert(reads == 0 && writes == 0 && status_reads == 0 && sleeps == 0);
}

static void scheduler_oversleep_does_not_consume_a_late_reply(void)
{
	struct t2sep_message result = { };
	reset();
	reply_count = 1;
	reply_ready[0] = sleep_ns = 6000 * NS_PER_MS;
	assert(t2sep_mailbox_receive_until(&mailbox, &result, 5000 * NS_PER_MS)
	       == -ETIMEDOUT);
	assert(sleeps == 1 && reads == 0 && reply_index == 0 && status_reads == 1);
}

static void scheduler_oversleep_does_not_post_a_late_request(void)
{
	struct t2sep_message request = { };
	reset();
	outbox_ready = sleep_ns = 6000 * NS_PER_MS;
	assert(t2sep_mailbox_send_until(&mailbox, &request, 5000 * NS_PER_MS)
	       == -ETIMEDOUT);
	assert(sleeps == 1 && writes == 0 && status_reads == 1);
}

static void unrelated_replies_do_not_renew_the_transaction(void)
{
	struct t2sep_message request = { }, result;
	reset();
	sleep_ns = 500 * NS_PER_MS;
	outbox_ready = 2000 * NS_PER_MS;
	reply_count = 3;
	reply_ready[0] = 3000 * NS_PER_MS;
	reply_ready[1] = 4500 * NS_PER_MS;
	reply_ready[2] = 5500 * NS_PER_MS;
	const ktime_t deadline = ktime_add_ms(ktime_get(), 5000);
	assert(t2sep_mailbox_send_until(&mailbox, &request, deadline) == 0);
	assert(now == 2000 * NS_PER_MS);
	assert(t2sep_mailbox_receive_until(&mailbox, &result, deadline) == 0);
	assert(now == 3000 * NS_PER_MS);
	assert(t2sep_mailbox_receive_until(&mailbox, &result, deadline) == 0);
	assert(now == 4500 * NS_PER_MS);
	assert(t2sep_mailbox_receive_until(&mailbox, &result, deadline) == -ETIMEDOUT);
	assert(now == 5000 * NS_PER_MS && reply_index == 2 && reads == 8);
}

static void matching_reply_before_deadline_is_received(void)
{
	struct t2sep_message result;
	reset();
	/* A long-running host clock must not overflow a microsecond counter. */
	now = 123456789000 * NS_PER_MS;
	sleep_ns = 500 * NS_PER_MS;
	reply_count = 1;
	reply_ready[0] = now + 4000 * NS_PER_MS;
	replies[0].word[0] = 0x12345678;
	assert(t2sep_mailbox_receive_until(&mailbox, &result,
		ktime_add_ms(ktime_get(), 5000)) == 0);
	assert(result.word[0] == 0x12345678 && reads == 4 && sleeps == 8);
}

int main(void)
{
	ready_and_fifo_order();
	expired_ready_descriptors_are_untouched();
	scheduler_oversleep_does_not_consume_a_late_reply();
	scheduler_oversleep_does_not_post_a_late_request();
	unrelated_replies_do_not_renew_the_transaction();
	matching_reply_before_deadline_is_received();
	puts("mailbox deadline tests: 6 passed (production C; no hardware)");
	return 0;
}

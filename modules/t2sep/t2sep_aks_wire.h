/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef T2SEP_AKS_WIRE_H
#define T2SEP_AKS_WIRE_H
#ifndef __KERNEL__
#include <stdbool.h>
#endif

/* The J152f SEPD MDMA transfer rejects external addresses with high32 >= 4.
 * AppleSEPManager's Intel allocator also supplies 34 address bits for OOL.
 * A 32-bit page index can encode 44 address bits; that capacity is not the
 * DMA allocation contract. See the manual-unlock static research notes.
 */
#define T2SEP_OOL_DMA_BITS 34U

static inline bool t2sep_ool_dma_range_valid(unsigned long long address,
					    unsigned long long length)
{
	const unsigned long long limit = 1ULL << T2SEP_OOL_DMA_BITS;

	/* Subtraction avoids wrapping address + length for malformed ranges. */
	return length && address < limit && length <= limit - address;
}

/* Initial capability replies may use the MBP16,1 0x50/v1 envelope.
 * The extra eight calendar bytes are not part of its v1 digest span.
 * Accept only documented layouts; never guess a digest after a mismatch.
 */
static inline unsigned int t2sep_aks_capability_payload_offset(
	unsigned int header_size, unsigned int version, unsigned int length)
{
	unsigned int offset;

	if (version != 1 || (header_size != 0x48 && header_size != 0x50))
		return 0;
	offset = 4 + header_size;
	return length >= offset + 16 && length <= 16384 ? offset : 0;
}

enum t2sep_aks_reply_kind {
	T2SEP_AKS_OTHER,
	T2SEP_AKS_COMPLETE,
	T2SEP_AKS_REJECTED,
};

static inline unsigned char t2sep_next_transaction(unsigned char *last)
{
	if (!++*last)
		++*last;
	return *last;
}

/* EP0 uses a nonzero tag in byte 1. The inspected SEPD responder sets
 * byte 2 to operation 1 and echoes the tag and byte-3 target. Require the full
 * endpoint byte too: aliases such as 0x20 are not control replies.
 */
static inline unsigned int t2sep_control_request_word(unsigned char opcode,
					      unsigned char target,
					      unsigned char tag)
{
	return ((unsigned int)tag << 8) | ((unsigned int)opcode << 16) |
	       ((unsigned int)target << 24);
}

static inline bool t2sep_control_reply_matches(unsigned int word,
					       unsigned char tag,
					       unsigned char target)
{
	return tag && (word & 0xff) == 0 && ((word >> 8) & 0xff) == tag &&
	       ((word >> 16) & 0xff) == 1 && (word >> 24) == target;
}

/* V1 capability negotiation accepts the legacy response-bit convention.
 * Normal V2 exchanges require the response bit and their exact transaction.
 */
static inline enum t2sep_aks_reply_kind
t2sep_aks_classify_reply(unsigned int word, unsigned char endpoint,
			unsigned char operation, unsigned char transaction,
			bool v1_negotiation)
{
	unsigned int reply_operation = (word >> 8) & 0xff;

	if (v1_negotiation && operation == 0x4d)
		reply_operation &= 0x7f;
	else
		operation |= 0x80;
	if ((word & 0xff) != endpoint || reply_operation != operation ||
	    ((word >> 16) & 0xff) != transaction)
		return T2SEP_AKS_OTHER;
	return word >> 24 ? T2SEP_AKS_REJECTED : T2SEP_AKS_COMPLETE;
}
#endif

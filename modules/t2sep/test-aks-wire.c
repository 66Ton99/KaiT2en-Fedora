// SPDX-License-Identifier: GPL-2.0-only
#include <assert.h>
#include <limits.h>
#include <stdio.h>
#include "t2sep_aks_wire.h"

int main(void)
{
	unsigned char last = 0, control = 0;
	unsigned char nop_tag, input_tag, output_tag;

	/* Full OOL ranges at the 16 GiB boundary, including overflow inputs. */
	assert(t2sep_ool_dma_range_valid(0, 0x4000));
	assert(t2sep_ool_dma_range_valid(0x3ffffc000ULL, 0x4000));
	assert(t2sep_ool_dma_range_valid(0x3ffffffffULL, 1));
	assert(!t2sep_ool_dma_range_valid(0x3fffff000ULL, 0x4000));
	assert(!t2sep_ool_dma_range_valid(0x400000000ULL, 0x4000));
	assert(!t2sep_ool_dma_range_valid(0x10000000000ULL, 0x4000));
	assert(!t2sep_ool_dma_range_valid(ULLONG_MAX, 0x4000));
	assert(!t2sep_ool_dma_range_valid(0x1000, ULLONG_MAX));
	assert(!t2sep_ool_dma_range_valid(0, 0));

	/* Fixed EP0 wire sequence: NOP, endpoint-7 input, endpoint-7 output. */
	nop_tag = t2sep_next_transaction(&control);
	input_tag = t2sep_next_transaction(&control);
	output_tag = t2sep_next_transaction(&control);
	assert(t2sep_control_request_word(0, 0, nop_tag) == 0x00000100);
	assert(t2sep_control_request_word(2, 7, input_tag) == 0x07020200);
	assert(t2sep_control_request_word(3, 7, output_tag) == 0x07030300);
	/* Fixed SEPD reply words: operation 1, echoed tag and target. */
	assert(t2sep_control_reply_matches(0x00010100, nop_tag, 0));
	assert(t2sep_control_reply_matches(0x07010200, input_tag, 7));
	assert(t2sep_control_reply_matches(0x07010300, output_tag, 7));
	assert(!t2sep_control_reply_matches(0x00010100, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x07010220, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x070102a0, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x07000200, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x07020200, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x08010200, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x00010200, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x07010300, input_tag, 7));
	assert(!t2sep_control_reply_matches(0x00010000, 0, 0));
	assert(last == 0); /* EP0 does not consume the independent EP7 sequence. */
	assert(t2sep_aks_capability_payload_offset(0x48, 1, 92) == 76);
	assert(t2sep_aks_capability_payload_offset(0x50, 1, 100) == 84);
	assert(!t2sep_aks_capability_payload_offset(0x50, 1, 99));
	assert(!t2sep_aks_capability_payload_offset(0x48, 1, 91));
	assert(!t2sep_aks_capability_payload_offset(0x50, 2, 100));
	assert(!t2sep_aks_capability_payload_offset(0x49, 1, 100));
	assert(!t2sep_aks_capability_payload_offset(0x50, 1, 16385));
	assert(t2sep_next_transaction(&last) == 1);
	assert(t2sep_next_transaction(&last) == 2);
	last = 254;
	assert(t2sep_next_transaction(&last) == 255);
	assert(t2sep_next_transaction(&last) == 1);
	// Fixed wire descriptors from the EP7 field contract, not a round trip.
	assert(t2sep_aks_classify_reply(0x0001cd07, 7, 0x4d, 1, true) == T2SEP_AKS_COMPLETE);
	assert(t2sep_aks_classify_reply(0x00014d07, 7, 0x4d, 1, true) == T2SEP_AKS_COMPLETE);
	assert(t2sep_aks_classify_reply(0x00014d07, 7, 0x4d, 1, false) == T2SEP_AKS_OTHER);
	assert(t2sep_aks_classify_reply(0x00028407, 7, 4, 2, false) == T2SEP_AKS_COMPLETE);
	assert(t2sep_aks_classify_reply(0xfb028407, 7, 4, 2, false) == T2SEP_AKS_REJECTED);
	assert(t2sep_aks_classify_reply(0x00020407, 7, 4, 2, false) == T2SEP_AKS_OTHER);
	assert(t2sep_aks_classify_reply(0x00018407, 7, 4, 2, false) == T2SEP_AKS_OTHER);
	assert(t2sep_aks_classify_reply(0x00028408, 7, 4, 2, false) == T2SEP_AKS_OTHER);
	assert(t2sep_aks_classify_reply(0x00028307, 7, 4, 2, false) == T2SEP_AKS_OTHER);
	puts("OOL DMA bounds, EP0 tags and EP7 response/status/transaction fixtures passed.");
	return 0;
}

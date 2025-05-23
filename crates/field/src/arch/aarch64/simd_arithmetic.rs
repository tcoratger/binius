// Copyright 2024-2025 Irreducible Inc.

use std::arch::aarch64::*;

use seq_macro::seq;

use super::m128::M128;
use crate::{
	BinaryField, TowerField,
	arch::{
		SimdStrategy,
		portable::packed_arithmetic::{
			PackedTowerField, TowerConstants, UnderlierWithBitConstants,
		},
	},
	arithmetic_traits::{
		MulAlpha, Square, TaggedInvertOrZero, TaggedMul, TaggedMulAlpha, TaggedSquare,
	},
	underlier::{UnderlierWithBitOps, WithUnderlier},
};

#[inline]
pub fn packed_tower_16x8b_multiply(a: M128, b: M128) -> M128 {
	let loga = lookup_16x8b(TOWER_LOG_LOOKUP_TABLE, a).into();
	let logb = lookup_16x8b(TOWER_LOG_LOOKUP_TABLE, b).into();
	let logc = unsafe {
		let sum = vaddq_u8(loga, logb);
		let overflow = vcgtq_u8(loga, sum);
		vsubq_u8(sum, overflow)
	};
	let c = lookup_16x8b(TOWER_EXP_LOOKUP_TABLE, logc.into()).into();
	unsafe {
		let a_or_b_is_0 = vorrq_u8(vceqzq_u8(a.into()), vceqzq_u8(b.into()));
		vandq_u8(c, veorq_u8(a_or_b_is_0, M128::fill_with_bit(1).into()))
	}
	.into()
}

#[inline]
pub fn packed_tower_16x8b_square(x: M128) -> M128 {
	lookup_16x8b(TOWER_SQUARE_LOOKUP_TABLE, x)
}

#[inline]
pub fn packed_tower_16x8b_invert_or_zero(x: M128) -> M128 {
	lookup_16x8b(TOWER_INVERT_OR_ZERO_LOOKUP_TABLE, x)
}

#[inline]
pub fn packed_tower_16x8b_multiply_alpha(x: M128) -> M128 {
	lookup_16x8b(TOWER_MUL_ALPHA_LOOKUP_TABLE, x)
}

#[inline]
pub fn packed_aes_16x8b_invert_or_zero(x: M128) -> M128 {
	lookup_16x8b(AES_INVERT_OR_ZERO_LOOKUP_TABLE, x)
}

#[inline]
pub fn packed_aes_16x8b_mul_alpha(x: M128) -> M128 {
	// 0xD3 corresponds to 0x10 after isomorphism from BinaryField8b to AESField
	packed_aes_16x8b_multiply(x, M128::from_le_bytes([0xD3; 16]))
}

#[inline]
pub fn packed_aes_16x8b_multiply(a: M128, b: M128) -> M128 {
	//! Performs a multiplication in GF(2^8) on the packed bytes.
	//! See https://doc.rust-lang.org/beta/core/arch/x86_64/fn._mm_gf2p8mul_epi8.html
	unsafe {
		let a = vreinterpretq_p8_p128(a.into());
		let b = vreinterpretq_p8_p128(b.into());
		let c0 = vreinterpretq_p8_p16(vmull_p8(vget_low_p8(a), vget_low_p8(b)));
		let c1 = vreinterpretq_p8_p16(vmull_p8(vget_high_p8(a), vget_high_p8(b)));

		// Reduces the 16-bit output of a carryless multiplication to 8 bits using equation 22 in
		// https://www.intel.com/content/dam/develop/external/us/en/documents/clmul-wp-rev-2-02-2014-04-20.pdf

		// Since q+(x) doesn't fit into 8 bits, we right shift the polynomial (divide by x) and
		// correct for this later. This works because q+(x) is divisible by x/the last polynomial
		// bit is 0. q+(x)/x = (x^8 + x^4 + x^3 + x)/x = 0b100011010 >> 1 = 0b10001101 = 0x8d
		const QPLUS_RSH1: poly8x8_t = unsafe { std::mem::transmute(0x8d8d8d8d8d8d8d8d_u64) };

		// q*(x) = x^4 + x^3 + x + 1 = 0b00011011 = 0x1b
		const QSTAR: poly8x8_t = unsafe { std::mem::transmute(0x1b1b1b1b1b1b1b1b_u64) };

		let cl = vuzp1q_p8(c0, c1);
		let ch = vuzp2q_p8(c0, c1);

		let tmp0 = vmull_p8(vget_low_p8(ch), QPLUS_RSH1);
		let tmp1 = vmull_p8(vget_high_p8(ch), QPLUS_RSH1);

		// Correct for q+(x) having beed divided by x
		let tmp0 = vreinterpretq_p8_u16(vshlq_n_u16(vreinterpretq_u16_p16(tmp0), 1));
		let tmp1 = vreinterpretq_p8_u16(vshlq_n_u16(vreinterpretq_u16_p16(tmp1), 1));

		let tmp_hi = vuzp2q_p8(tmp0, tmp1);
		let tmp0 = vreinterpretq_p8_p16(vmull_p8(vget_low_p8(tmp_hi), QSTAR));
		let tmp1 = vreinterpretq_p8_p16(vmull_p8(vget_high_p8(tmp_hi), QSTAR));
		let tmp_lo = vuzp1q_p8(tmp0, tmp1);

		vreinterpretq_p128_p8(vaddq_p8(cl, tmp_lo)).into()
	}
}

#[inline]
pub fn packed_tower_16x8b_into_aes(x: M128) -> M128 {
	lookup_16x8b(TOWER_TO_AES_LOOKUP_TABLE, x)
}

#[inline]
pub fn packed_aes_16x8b_into_tower(x: M128) -> M128 {
	lookup_16x8b(AES_TO_TOWER_LOOKUP_TABLE, x)
}

#[inline]
pub fn lookup_16x8b(table: [u8; 256], x: M128) -> M128 {
	unsafe {
		let table: [uint8x16x4_t; 4] = std::mem::transmute(table);
		let x = x.into();
		let y0 = vqtbl4q_u8(table[0], x);
		let y1 = vqtbl4q_u8(table[1], veorq_u8(x, vdupq_n_u8(0x40)));
		let y2 = vqtbl4q_u8(table[2], veorq_u8(x, vdupq_n_u8(0x80)));
		let y3 = vqtbl4q_u8(table[3], veorq_u8(x, vdupq_n_u8(0xC0)));
		veorq_u8(veorq_u8(y0, y1), veorq_u8(y2, y3)).into()
	}
}

pub const TOWER_TO_AES_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x01, 0xBC, 0xBD, 0xB0, 0xB1, 0x0C, 0x0D, 0xEC, 0xED, 0x50, 0x51, 0x5C, 0x5D, 0xE0, 0xE1,
	0xD3, 0xD2, 0x6F, 0x6E, 0x63, 0x62, 0xDF, 0xDE, 0x3F, 0x3E, 0x83, 0x82, 0x8F, 0x8E, 0x33, 0x32,
	0x8D, 0x8C, 0x31, 0x30, 0x3D, 0x3C, 0x81, 0x80, 0x61, 0x60, 0xDD, 0xDC, 0xD1, 0xD0, 0x6D, 0x6C,
	0x5E, 0x5F, 0xE2, 0xE3, 0xEE, 0xEF, 0x52, 0x53, 0xB2, 0xB3, 0x0E, 0x0F, 0x02, 0x03, 0xBE, 0xBF,
	0x2E, 0x2F, 0x92, 0x93, 0x9E, 0x9F, 0x22, 0x23, 0xC2, 0xC3, 0x7E, 0x7F, 0x72, 0x73, 0xCE, 0xCF,
	0xFD, 0xFC, 0x41, 0x40, 0x4D, 0x4C, 0xF1, 0xF0, 0x11, 0x10, 0xAD, 0xAC, 0xA1, 0xA0, 0x1D, 0x1C,
	0xA3, 0xA2, 0x1F, 0x1E, 0x13, 0x12, 0xAF, 0xAE, 0x4F, 0x4E, 0xF3, 0xF2, 0xFF, 0xFE, 0x43, 0x42,
	0x70, 0x71, 0xCC, 0xCD, 0xC0, 0xC1, 0x7C, 0x7D, 0x9C, 0x9D, 0x20, 0x21, 0x2C, 0x2D, 0x90, 0x91,
	0x58, 0x59, 0xE4, 0xE5, 0xE8, 0xE9, 0x54, 0x55, 0xB4, 0xB5, 0x08, 0x09, 0x04, 0x05, 0xB8, 0xB9,
	0x8B, 0x8A, 0x37, 0x36, 0x3B, 0x3A, 0x87, 0x86, 0x67, 0x66, 0xDB, 0xDA, 0xD7, 0xD6, 0x6B, 0x6A,
	0xD5, 0xD4, 0x69, 0x68, 0x65, 0x64, 0xD9, 0xD8, 0x39, 0x38, 0x85, 0x84, 0x89, 0x88, 0x35, 0x34,
	0x06, 0x07, 0xBA, 0xBB, 0xB6, 0xB7, 0x0A, 0x0B, 0xEA, 0xEB, 0x56, 0x57, 0x5A, 0x5B, 0xE6, 0xE7,
	0x76, 0x77, 0xCA, 0xCB, 0xC6, 0xC7, 0x7A, 0x7B, 0x9A, 0x9B, 0x26, 0x27, 0x2A, 0x2B, 0x96, 0x97,
	0xA5, 0xA4, 0x19, 0x18, 0x15, 0x14, 0xA9, 0xA8, 0x49, 0x48, 0xF5, 0xF4, 0xF9, 0xF8, 0x45, 0x44,
	0xFB, 0xFA, 0x47, 0x46, 0x4B, 0x4A, 0xF7, 0xF6, 0x17, 0x16, 0xAB, 0xAA, 0xA7, 0xA6, 0x1B, 0x1A,
	0x28, 0x29, 0x94, 0x95, 0x98, 0x99, 0x24, 0x25, 0xC4, 0xC5, 0x78, 0x79, 0x74, 0x75, 0xC8, 0xC9,
];

pub const AES_TO_TOWER_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x01, 0x3C, 0x3D, 0x8C, 0x8D, 0xB0, 0xB1, 0x8A, 0x8B, 0xB6, 0xB7, 0x06, 0x07, 0x3A, 0x3B,
	0x59, 0x58, 0x65, 0x64, 0xD5, 0xD4, 0xE9, 0xE8, 0xD3, 0xD2, 0xEF, 0xEE, 0x5F, 0x5E, 0x63, 0x62,
	0x7A, 0x7B, 0x46, 0x47, 0xF6, 0xF7, 0xCA, 0xCB, 0xF0, 0xF1, 0xCC, 0xCD, 0x7C, 0x7D, 0x40, 0x41,
	0x23, 0x22, 0x1F, 0x1E, 0xAF, 0xAE, 0x93, 0x92, 0xA9, 0xA8, 0x95, 0x94, 0x25, 0x24, 0x19, 0x18,
	0x53, 0x52, 0x6F, 0x6E, 0xDF, 0xDE, 0xE3, 0xE2, 0xD9, 0xD8, 0xE5, 0xE4, 0x55, 0x54, 0x69, 0x68,
	0x0A, 0x0B, 0x36, 0x37, 0x86, 0x87, 0xBA, 0xBB, 0x80, 0x81, 0xBC, 0xBD, 0x0C, 0x0D, 0x30, 0x31,
	0x29, 0x28, 0x15, 0x14, 0xA5, 0xA4, 0x99, 0x98, 0xA3, 0xA2, 0x9F, 0x9E, 0x2F, 0x2E, 0x13, 0x12,
	0x70, 0x71, 0x4C, 0x4D, 0xFC, 0xFD, 0xC0, 0xC1, 0xFA, 0xFB, 0xC6, 0xC7, 0x76, 0x77, 0x4A, 0x4B,
	0x27, 0x26, 0x1B, 0x1A, 0xAB, 0xAA, 0x97, 0x96, 0xAD, 0xAC, 0x91, 0x90, 0x21, 0x20, 0x1D, 0x1C,
	0x7E, 0x7F, 0x42, 0x43, 0xF2, 0xF3, 0xCE, 0xCF, 0xF4, 0xF5, 0xC8, 0xC9, 0x78, 0x79, 0x44, 0x45,
	0x5D, 0x5C, 0x61, 0x60, 0xD1, 0xD0, 0xED, 0xEC, 0xD7, 0xD6, 0xEB, 0xEA, 0x5B, 0x5A, 0x67, 0x66,
	0x04, 0x05, 0x38, 0x39, 0x88, 0x89, 0xB4, 0xB5, 0x8E, 0x8F, 0xB2, 0xB3, 0x02, 0x03, 0x3E, 0x3F,
	0x74, 0x75, 0x48, 0x49, 0xF8, 0xF9, 0xC4, 0xC5, 0xFE, 0xFF, 0xC2, 0xC3, 0x72, 0x73, 0x4E, 0x4F,
	0x2D, 0x2C, 0x11, 0x10, 0xA1, 0xA0, 0x9D, 0x9C, 0xA7, 0xA6, 0x9B, 0x9A, 0x2B, 0x2A, 0x17, 0x16,
	0x0E, 0x0F, 0x32, 0x33, 0x82, 0x83, 0xBE, 0xBF, 0x84, 0x85, 0xB8, 0xB9, 0x08, 0x09, 0x34, 0x35,
	0x57, 0x56, 0x6B, 0x6A, 0xDB, 0xDA, 0xE7, 0xE6, 0xDD, 0xDC, 0xE1, 0xE0, 0x51, 0x50, 0x6D, 0x6C,
];

pub const TOWER_SQUARE_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x01, 0x03, 0x02, 0x09, 0x08, 0x0A, 0x0B, 0x07, 0x06, 0x04, 0x05, 0x0E, 0x0F, 0x0D, 0x0C,
	0x41, 0x40, 0x42, 0x43, 0x48, 0x49, 0x4B, 0x4A, 0x46, 0x47, 0x45, 0x44, 0x4F, 0x4E, 0x4C, 0x4D,
	0xC3, 0xC2, 0xC0, 0xC1, 0xCA, 0xCB, 0xC9, 0xC8, 0xC4, 0xC5, 0xC7, 0xC6, 0xCD, 0xCC, 0xCE, 0xCF,
	0x82, 0x83, 0x81, 0x80, 0x8B, 0x8A, 0x88, 0x89, 0x85, 0x84, 0x86, 0x87, 0x8C, 0x8D, 0x8F, 0x8E,
	0xA9, 0xA8, 0xAA, 0xAB, 0xA0, 0xA1, 0xA3, 0xA2, 0xAE, 0xAF, 0xAD, 0xAC, 0xA7, 0xA6, 0xA4, 0xA5,
	0xE8, 0xE9, 0xEB, 0xEA, 0xE1, 0xE0, 0xE2, 0xE3, 0xEF, 0xEE, 0xEC, 0xED, 0xE6, 0xE7, 0xE5, 0xE4,
	0x6A, 0x6B, 0x69, 0x68, 0x63, 0x62, 0x60, 0x61, 0x6D, 0x6C, 0x6E, 0x6F, 0x64, 0x65, 0x67, 0x66,
	0x2B, 0x2A, 0x28, 0x29, 0x22, 0x23, 0x21, 0x20, 0x2C, 0x2D, 0x2F, 0x2E, 0x25, 0x24, 0x26, 0x27,
	0x57, 0x56, 0x54, 0x55, 0x5E, 0x5F, 0x5D, 0x5C, 0x50, 0x51, 0x53, 0x52, 0x59, 0x58, 0x5A, 0x5B,
	0x16, 0x17, 0x15, 0x14, 0x1F, 0x1E, 0x1C, 0x1D, 0x11, 0x10, 0x12, 0x13, 0x18, 0x19, 0x1B, 0x1A,
	0x94, 0x95, 0x97, 0x96, 0x9D, 0x9C, 0x9E, 0x9F, 0x93, 0x92, 0x90, 0x91, 0x9A, 0x9B, 0x99, 0x98,
	0xD5, 0xD4, 0xD6, 0xD7, 0xDC, 0xDD, 0xDF, 0xDE, 0xD2, 0xD3, 0xD1, 0xD0, 0xDB, 0xDA, 0xD8, 0xD9,
	0xFE, 0xFF, 0xFD, 0xFC, 0xF7, 0xF6, 0xF4, 0xF5, 0xF9, 0xF8, 0xFA, 0xFB, 0xF0, 0xF1, 0xF3, 0xF2,
	0xBF, 0xBE, 0xBC, 0xBD, 0xB6, 0xB7, 0xB5, 0xB4, 0xB8, 0xB9, 0xBB, 0xBA, 0xB1, 0xB0, 0xB2, 0xB3,
	0x3D, 0x3C, 0x3E, 0x3F, 0x34, 0x35, 0x37, 0x36, 0x3A, 0x3B, 0x39, 0x38, 0x33, 0x32, 0x30, 0x31,
	0x7C, 0x7D, 0x7F, 0x7E, 0x75, 0x74, 0x76, 0x77, 0x7B, 0x7A, 0x78, 0x79, 0x72, 0x73, 0x71, 0x70,
];

pub const TOWER_INVERT_OR_ZERO_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x01, 0x03, 0x02, 0x06, 0x0E, 0x04, 0x0F, 0x0D, 0x0A, 0x09, 0x0C, 0x0B, 0x08, 0x05, 0x07,
	0x14, 0x67, 0x94, 0x7B, 0x10, 0x66, 0x9E, 0x7E, 0xD2, 0x81, 0x27, 0x4B, 0xD1, 0x8F, 0x2F, 0x42,
	0x3C, 0xE6, 0xDE, 0x7C, 0xB3, 0xC1, 0x4A, 0x1A, 0x30, 0xE9, 0xDD, 0x79, 0xB1, 0xC6, 0x43, 0x1E,
	0x28, 0xE8, 0x9D, 0xB9, 0x63, 0x39, 0x8D, 0xC2, 0x62, 0x35, 0x83, 0xC5, 0x20, 0xE7, 0x97, 0xBB,
	0x61, 0x48, 0x1F, 0x2E, 0xAC, 0xC8, 0xBC, 0x56, 0x41, 0x60, 0x26, 0x1B, 0xCF, 0xAA, 0x5B, 0xBE,
	0xEF, 0x73, 0x6D, 0x5E, 0xF7, 0x86, 0x47, 0xBD, 0x88, 0xFC, 0xBF, 0x4E, 0x76, 0xE0, 0x53, 0x6C,
	0x49, 0x40, 0x38, 0x34, 0xE4, 0xEB, 0x15, 0x11, 0x8B, 0x85, 0xAF, 0xA9, 0x5F, 0x52, 0x98, 0x92,
	0xFB, 0xB5, 0xEE, 0x51, 0xB7, 0xF0, 0x5C, 0xE1, 0xDC, 0x2B, 0x95, 0x13, 0x23, 0xDF, 0x17, 0x9F,
	0xD3, 0x19, 0xC4, 0x3A, 0x8A, 0x69, 0x55, 0xF6, 0x58, 0xFD, 0x84, 0x68, 0xC3, 0x36, 0xD0, 0x1D,
	0xA6, 0xF3, 0x6F, 0x99, 0x12, 0x7A, 0xBA, 0x3E, 0x6E, 0x93, 0xA0, 0xF8, 0xB8, 0x32, 0x16, 0x7F,
	0x9A, 0xF9, 0xE2, 0xDB, 0xED, 0xD8, 0x90, 0xF2, 0xAE, 0x6B, 0x4D, 0xCE, 0x44, 0xC9, 0xA8, 0x6A,
	0xC7, 0x2C, 0xC0, 0x24, 0xFA, 0x71, 0xF1, 0x74, 0x9C, 0x33, 0x96, 0x3F, 0x46, 0x57, 0x4F, 0x5A,
	0xB2, 0x25, 0x37, 0x8C, 0x82, 0x3B, 0x2D, 0xB0, 0x45, 0xAD, 0xD7, 0xFF, 0xF4, 0xD4, 0xAB, 0x4C,
	0x8E, 0x1C, 0x18, 0x80, 0xCD, 0xF5, 0xFE, 0xCA, 0xA5, 0xEC, 0xE3, 0xA3, 0x78, 0x2A, 0x22, 0x7D,
	0x5D, 0x77, 0xA2, 0xDA, 0x64, 0xEA, 0x21, 0x3D, 0x31, 0x29, 0xE5, 0x65, 0xD9, 0xA4, 0x72, 0x50,
	0x75, 0xB6, 0xA7, 0x91, 0xCC, 0xD5, 0x87, 0x54, 0x9B, 0xA1, 0xB4, 0x70, 0x59, 0x89, 0xD6, 0xCB,
];

pub const TOWER_MUL_ALPHA_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0, 0xF0,
	0x41, 0x51, 0x61, 0x71, 0x01, 0x11, 0x21, 0x31, 0xC1, 0xD1, 0xE1, 0xF1, 0x81, 0x91, 0xA1, 0xB1,
	0x82, 0x92, 0xA2, 0xB2, 0xC2, 0xD2, 0xE2, 0xF2, 0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72,
	0xC3, 0xD3, 0xE3, 0xF3, 0x83, 0x93, 0xA3, 0xB3, 0x43, 0x53, 0x63, 0x73, 0x03, 0x13, 0x23, 0x33,
	0x94, 0x84, 0xB4, 0xA4, 0xD4, 0xC4, 0xF4, 0xE4, 0x14, 0x04, 0x34, 0x24, 0x54, 0x44, 0x74, 0x64,
	0xD5, 0xC5, 0xF5, 0xE5, 0x95, 0x85, 0xB5, 0xA5, 0x55, 0x45, 0x75, 0x65, 0x15, 0x05, 0x35, 0x25,
	0x16, 0x06, 0x36, 0x26, 0x56, 0x46, 0x76, 0x66, 0x96, 0x86, 0xB6, 0xA6, 0xD6, 0xC6, 0xF6, 0xE6,
	0x57, 0x47, 0x77, 0x67, 0x17, 0x07, 0x37, 0x27, 0xD7, 0xC7, 0xF7, 0xE7, 0x97, 0x87, 0xB7, 0xA7,
	0xE8, 0xF8, 0xC8, 0xD8, 0xA8, 0xB8, 0x88, 0x98, 0x68, 0x78, 0x48, 0x58, 0x28, 0x38, 0x08, 0x18,
	0xA9, 0xB9, 0x89, 0x99, 0xE9, 0xF9, 0xC9, 0xD9, 0x29, 0x39, 0x09, 0x19, 0x69, 0x79, 0x49, 0x59,
	0x6A, 0x7A, 0x4A, 0x5A, 0x2A, 0x3A, 0x0A, 0x1A, 0xEA, 0xFA, 0xCA, 0xDA, 0xAA, 0xBA, 0x8A, 0x9A,
	0x2B, 0x3B, 0x0B, 0x1B, 0x6B, 0x7B, 0x4B, 0x5B, 0xAB, 0xBB, 0x8B, 0x9B, 0xEB, 0xFB, 0xCB, 0xDB,
	0x7C, 0x6C, 0x5C, 0x4C, 0x3C, 0x2C, 0x1C, 0x0C, 0xFC, 0xEC, 0xDC, 0xCC, 0xBC, 0xAC, 0x9C, 0x8C,
	0x3D, 0x2D, 0x1D, 0x0D, 0x7D, 0x6D, 0x5D, 0x4D, 0xBD, 0xAD, 0x9D, 0x8D, 0xFD, 0xED, 0xDD, 0xCD,
	0xFE, 0xEE, 0xDE, 0xCE, 0xBE, 0xAE, 0x9E, 0x8E, 0x7E, 0x6E, 0x5E, 0x4E, 0x3E, 0x2E, 0x1E, 0x0E,
	0xBF, 0xAF, 0x9F, 0x8F, 0xFF, 0xEF, 0xDF, 0xCF, 0x3F, 0x2F, 0x1F, 0x0F, 0x7F, 0x6F, 0x5F, 0x4F,
];

pub const AES_INVERT_OR_ZERO_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x01, 0x8D, 0xF6, 0xCB, 0x52, 0x7B, 0xD1, 0xE8, 0x4F, 0x29, 0xC0, 0xB0, 0xE1, 0xE5, 0xC7,
	0x74, 0xB4, 0xAA, 0x4B, 0x99, 0x2B, 0x60, 0x5F, 0x58, 0x3F, 0xFD, 0xCC, 0xFF, 0x40, 0xEE, 0xB2,
	0x3A, 0x6E, 0x5A, 0xF1, 0x55, 0x4D, 0xA8, 0xC9, 0xC1, 0x0A, 0x98, 0x15, 0x30, 0x44, 0xA2, 0xC2,
	0x2C, 0x45, 0x92, 0x6C, 0xF3, 0x39, 0x66, 0x42, 0xF2, 0x35, 0x20, 0x6F, 0x77, 0xBB, 0x59, 0x19,
	0x1D, 0xFE, 0x37, 0x67, 0x2D, 0x31, 0xF5, 0x69, 0xA7, 0x64, 0xAB, 0x13, 0x54, 0x25, 0xE9, 0x09,
	0xED, 0x5C, 0x05, 0xCA, 0x4C, 0x24, 0x87, 0xBF, 0x18, 0x3E, 0x22, 0xF0, 0x51, 0xEC, 0x61, 0x17,
	0x16, 0x5E, 0xAF, 0xD3, 0x49, 0xA6, 0x36, 0x43, 0xF4, 0x47, 0x91, 0xDF, 0x33, 0x93, 0x21, 0x3B,
	0x79, 0xB7, 0x97, 0x85, 0x10, 0xB5, 0xBA, 0x3C, 0xB6, 0x70, 0xD0, 0x06, 0xA1, 0xFA, 0x81, 0x82,
	0x83, 0x7E, 0x7F, 0x80, 0x96, 0x73, 0xBE, 0x56, 0x9B, 0x9E, 0x95, 0xD9, 0xF7, 0x02, 0xB9, 0xA4,
	0xDE, 0x6A, 0x32, 0x6D, 0xD8, 0x8A, 0x84, 0x72, 0x2A, 0x14, 0x9F, 0x88, 0xF9, 0xDC, 0x89, 0x9A,
	0xFB, 0x7C, 0x2E, 0xC3, 0x8F, 0xB8, 0x65, 0x48, 0x26, 0xC8, 0x12, 0x4A, 0xCE, 0xE7, 0xD2, 0x62,
	0x0C, 0xE0, 0x1F, 0xEF, 0x11, 0x75, 0x78, 0x71, 0xA5, 0x8E, 0x76, 0x3D, 0xBD, 0xBC, 0x86, 0x57,
	0x0B, 0x28, 0x2F, 0xA3, 0xDA, 0xD4, 0xE4, 0x0F, 0xA9, 0x27, 0x53, 0x04, 0x1B, 0xFC, 0xAC, 0xE6,
	0x7A, 0x07, 0xAE, 0x63, 0xC5, 0xDB, 0xE2, 0xEA, 0x94, 0x8B, 0xC4, 0xD5, 0x9D, 0xF8, 0x90, 0x6B,
	0xB1, 0x0D, 0xD6, 0xEB, 0xC6, 0x0E, 0xCF, 0xAD, 0x08, 0x4E, 0xD7, 0xE3, 0x5D, 0x50, 0x1E, 0xB3,
	0x5B, 0x23, 0x38, 0x34, 0x68, 0x46, 0x03, 0x8C, 0xDD, 0x9C, 0x7D, 0xA0, 0xCD, 0x1A, 0x41, 0x1C,
];

pub const TOWER_EXP_LOOKUP_TABLE: [u8; 256] = [
	0x01, 0x13, 0x43, 0x66, 0xAB, 0x8C, 0x60, 0xC6, 0x91, 0xCA, 0x59, 0xB2, 0x6A, 0x63, 0xF4, 0x53,
	0x17, 0x0F, 0xFA, 0xBA, 0xEE, 0x87, 0xD6, 0xE0, 0x6E, 0x2F, 0x68, 0x42, 0x75, 0xE8, 0xEA, 0xCB,
	0x4A, 0xF1, 0x0C, 0xC8, 0x78, 0x33, 0xD1, 0x9E, 0x30, 0xE3, 0x5C, 0xED, 0xB5, 0x14, 0x3D, 0x38,
	0x67, 0xB8, 0xCF, 0x06, 0x6D, 0x1D, 0xAA, 0x9F, 0x23, 0xA0, 0x3A, 0x46, 0x39, 0x74, 0xFB, 0xA9,
	0xAD, 0xE1, 0x7D, 0x6C, 0x0E, 0xE9, 0xF9, 0x88, 0x2C, 0x5A, 0x80, 0xA8, 0xBE, 0xA2, 0x1B, 0xC7,
	0x82, 0x89, 0x3F, 0x19, 0xE6, 0x03, 0x32, 0xC2, 0xDD, 0x56, 0x48, 0xD0, 0x8D, 0x73, 0x85, 0xF7,
	0x61, 0xD5, 0xD2, 0xAC, 0xF2, 0x3E, 0x0A, 0xA5, 0x65, 0x99, 0x4E, 0xBD, 0x90, 0xD9, 0x1A, 0xD4,
	0xC1, 0xEF, 0x94, 0x95, 0x86, 0xC5, 0xA3, 0x08, 0x84, 0xE4, 0x22, 0xB3, 0x79, 0x20, 0x92, 0xF8,
	0x9B, 0x6F, 0x3C, 0x2B, 0x24, 0xDE, 0x64, 0x8A, 0x0D, 0xDB, 0x3B, 0x55, 0x7A, 0x12, 0x50, 0x25,
	0xCD, 0x27, 0xEC, 0xA6, 0x57, 0x5B, 0x93, 0xEB, 0xD8, 0x09, 0x97, 0xA7, 0x44, 0x18, 0xF5, 0x40,
	0x54, 0x69, 0x51, 0x36, 0x8E, 0x41, 0x47, 0x2A, 0x37, 0x9D, 0x02, 0x21, 0x81, 0xBB, 0xFD, 0xC4,
	0xB0, 0x4B, 0xE2, 0x4F, 0xAE, 0xD3, 0xBF, 0xB1, 0x58, 0xA1, 0x29, 0x05, 0x5F, 0xDF, 0x77, 0xC9,
	0x6B, 0x70, 0xB7, 0x35, 0xBC, 0x83, 0x9A, 0x7C, 0x7F, 0x4D, 0x8F, 0x52, 0x04, 0x4C, 0x9C, 0x11,
	0x62, 0xE7, 0x10, 0x71, 0xA4, 0x76, 0xDA, 0x28, 0x16, 0x1C, 0xB9, 0xDC, 0x45, 0x0B, 0xB6, 0x26,
	0xFF, 0xE5, 0x31, 0xF0, 0x1F, 0x8B, 0x1E, 0x98, 0x5D, 0xFE, 0xF6, 0x72, 0x96, 0xB4, 0x07, 0x7E,
	0x5E, 0xCC, 0x34, 0xAF, 0xC0, 0xFC, 0xD7, 0xF3, 0x2D, 0x49, 0xC3, 0xCE, 0x15, 0x2E, 0x7B, 0x01,
];

pub const TOWER_LOG_LOOKUP_TABLE: [u8; 256] = [
	0x00, 0x00, 0xAA, 0x55, 0xCC, 0xBB, 0x33, 0xEE, 0x77, 0x99, 0x66, 0xDD, 0x22, 0x88, 0x44, 0x11,
	0xD2, 0xCF, 0x8D, 0x01, 0x2D, 0xFC, 0xD8, 0x10, 0x9D, 0x53, 0x6E, 0x4E, 0xD9, 0x35, 0xE6, 0xE4,
	0x7D, 0xAB, 0x7A, 0x38, 0x84, 0x8F, 0xDF, 0x91, 0xD7, 0xBA, 0xA7, 0x83, 0x48, 0xF8, 0xFD, 0x19,
	0x28, 0xE2, 0x56, 0x25, 0xF2, 0xC3, 0xA3, 0xA8, 0x2F, 0x3C, 0x3A, 0x8A, 0x82, 0x2E, 0x65, 0x52,
	0x9F, 0xA5, 0x1B, 0x02, 0x9C, 0xDC, 0x3B, 0xA6, 0x5A, 0xF9, 0x20, 0xB1, 0xCD, 0xC9, 0x6A, 0xB3,
	0x8E, 0xA2, 0xCB, 0x0F, 0xA0, 0x8B, 0x59, 0x94, 0xB8, 0x0A, 0x49, 0x95, 0x2A, 0xE8, 0xF0, 0xBC,
	0x06, 0x60, 0xD0, 0x0D, 0x86, 0x68, 0x03, 0x30, 0x1A, 0xA1, 0x0C, 0xC0, 0x43, 0x34, 0x18, 0x81,
	0xC1, 0xD3, 0xEB, 0x5D, 0x3D, 0x1C, 0xD5, 0xBE, 0x24, 0x7C, 0x8C, 0xFE, 0xC7, 0x42, 0xEF, 0xC8,
	0x4A, 0xAC, 0x50, 0xC5, 0x78, 0x5E, 0x74, 0x15, 0x47, 0x51, 0x87, 0xE5, 0x05, 0x5C, 0xA4, 0xCA,
	0x6C, 0x08, 0x7E, 0x96, 0x72, 0x73, 0xEC, 0x9A, 0xE7, 0x69, 0xC6, 0x80, 0xCE, 0xA9, 0x27, 0x37,
	0x39, 0xB9, 0x4D, 0x76, 0xD4, 0x67, 0x93, 0x9B, 0x4B, 0x3F, 0x36, 0x04, 0x63, 0x40, 0xB4, 0xF3,
	0xB0, 0xB7, 0x0B, 0x7B, 0xED, 0x2C, 0xDE, 0xC2, 0x31, 0xDA, 0x13, 0xAD, 0xC4, 0x6B, 0x4C, 0xB6,
	0xF4, 0x70, 0x57, 0xFA, 0xAF, 0x75, 0x07, 0x4F, 0x23, 0xBF, 0x09, 0x1F, 0xF1, 0x90, 0xFB, 0x32,
	0x5B, 0x26, 0x62, 0xB5, 0x6F, 0x61, 0x16, 0xF6, 0x98, 0x6D, 0xD6, 0x89, 0xDB, 0x58, 0x85, 0xBD,
	0x17, 0x41, 0xB2, 0x29, 0x79, 0xE1, 0x54, 0xD1, 0x1D, 0x45, 0x1E, 0x97, 0x92, 0x2B, 0x14, 0x71,
	0xE3, 0x21, 0x64, 0xF7, 0x0E, 0x9E, 0xEA, 0x5F, 0x7F, 0x46, 0x12, 0x3E, 0xF5, 0xAE, 0xE9, 0xE0,
];

impl<PT> TaggedMul<SimdStrategy> for PT
where
	PT: PackedTowerField<Underlier = M128>,
	PT::DirectSubfield: TowerConstants<M128> + BinaryField,
{
	#[inline]
	fn mul(self, rhs: Self) -> Self {
		let alphas = PT::DirectSubfield::ALPHAS_ODD;
		let odd_mask = M128::INTERLEAVE_ODD_MASK[PT::DirectSubfield::TOWER_LEVEL];
		let a = self.as_packed_subfield();
		let b = rhs.as_packed_subfield();
		let p1 = (a * b).to_underlier();
		let (lo, hi) =
			M128::interleave(a.to_underlier(), b.to_underlier(), PT::DirectSubfield::TOWER_LEVEL);
		let (lhs, rhs) =
			M128::interleave(lo ^ hi, alphas ^ (p1 & odd_mask), PT::DirectSubfield::TOWER_LEVEL);
		let p2 = (PT::PackedDirectSubfield::from_underlier(lhs)
			* PT::PackedDirectSubfield::from_underlier(rhs))
		.to_underlier();
		let q1 = p1 ^ flip_even_odd::<PT::DirectSubfield>(p1);
		let q2 = p2 ^ shift_left::<PT::DirectSubfield>(p2);
		Self::from_underlier(q1 ^ (q2 & odd_mask))
	}
}

impl<PT> TaggedMulAlpha<SimdStrategy> for PT
where
	PT: PackedTowerField<Underlier = M128>,
	PT::PackedDirectSubfield: MulAlpha,
{
	#[inline]
	fn mul_alpha(self) -> Self {
		let a0_a1 = self.as_packed_subfield();
		let a0alpha_a1alpha: M128 = a0_a1.mul_alpha().to_underlier();
		let a1_a0 = flip_even_odd::<PT::DirectSubfield>(a0_a1.to_underlier());
		Self::from_underlier(blend_odd_even::<PT::DirectSubfield>(a1_a0 ^ a0alpha_a1alpha, a1_a0))
	}
}

impl<PT> TaggedSquare<SimdStrategy> for PT
where
	PT: PackedTowerField<Underlier = M128>,
	PT::PackedDirectSubfield: MulAlpha + Square,
{
	#[inline]
	fn square(self) -> Self {
		let a0_a1 = self.as_packed_subfield();
		let a0sq_a1sq = Square::square(a0_a1);
		let a1sq_a0sq = flip_even_odd::<PT::DirectSubfield>(a0sq_a1sq.to_underlier());
		let a0sq_plus_a1sq = a0sq_a1sq.to_underlier() ^ a1sq_a0sq;
		let a1_mul_alpha = a0sq_a1sq.mul_alpha();
		Self::from_underlier(blend_odd_even::<PT::DirectSubfield>(
			a1_mul_alpha.to_underlier(),
			a0sq_plus_a1sq,
		))
	}
}

impl<PT> TaggedInvertOrZero<SimdStrategy> for PT
where
	PT: PackedTowerField<Underlier = M128>,
	PT::PackedDirectSubfield: MulAlpha + Square,
{
	#[inline]
	fn invert_or_zero(self) -> Self {
		let a0_a1 = self.as_packed_subfield();
		let a1_a0 = a0_a1.mutate_underlier(flip_even_odd::<PT::DirectSubfield>);
		let a1alpha = a1_a0.mul_alpha();
		let a0_plus_a1alpha = a0_a1 + a1alpha;
		let a1sq_a0sq = Square::square(a1_a0);
		let delta = a1sq_a0sq + (a0_plus_a1alpha * a0_a1);
		let deltainv = delta.invert_or_zero();
		let deltainv_deltainv = deltainv.mutate_underlier(duplicate_odd::<PT::DirectSubfield>);
		let delta_multiplier = a0_a1.mutate_underlier(|a0_a1| {
			blend_odd_even::<PT::DirectSubfield>(a0_a1, a0_plus_a1alpha.to_underlier())
		});
		PT::from_packed_subfield(deltainv_deltainv * delta_multiplier)
	}
}

#[inline]
fn duplicate_odd<F: TowerField>(x: M128) -> M128 {
	match F::TOWER_LEVEL {
		0..=2 => {
			let t = x & M128::INTERLEAVE_ODD_MASK[F::TOWER_LEVEL];
			t | shift_right::<F>(t)
		}
		3 => x.shuffle_u8([1, 1, 3, 3, 5, 5, 7, 7, 9, 9, 11, 11, 13, 13, 15, 15]),
		4 => x.shuffle_u8([2, 3, 2, 3, 6, 7, 6, 7, 10, 11, 10, 11, 14, 15, 14, 15]),
		5 => x.shuffle_u8([4, 5, 6, 7, 4, 5, 6, 7, 12, 13, 14, 15, 12, 13, 14, 15]),
		6 => x.shuffle_u8([8, 9, 10, 11, 12, 13, 14, 15, 8, 9, 10, 11, 12, 13, 14, 15]),
		_ => panic!("Unsupported tower level"),
	}
}

#[inline]
fn flip_even_odd<F: TowerField>(x: M128) -> M128 {
	match F::TOWER_LEVEL {
		0..=2 => {
			let m1 = M128::INTERLEAVE_ODD_MASK[F::TOWER_LEVEL];
			let m2 = M128::INTERLEAVE_EVEN_MASK[F::TOWER_LEVEL];
			shift_right::<F>(x & m1) | shift_left::<F>(x & m2)
		}
		3 => x.shuffle_u8([1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14]),
		4 => x.shuffle_u8([2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13]),
		5 => x.shuffle_u8([4, 5, 6, 7, 0, 1, 2, 3, 12, 13, 14, 15, 8, 9, 10, 11]),
		6 => x.shuffle_u8([8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7]),
		_ => panic!("Unsupported tower level"),
	}
}

#[inline]
fn blend_odd_even<F: TowerField>(x: M128, y: M128) -> M128 {
	let m1 = M128::INTERLEAVE_ODD_MASK[F::TOWER_LEVEL];
	let m2 = M128::INTERLEAVE_EVEN_MASK[F::TOWER_LEVEL];
	(x & m1) | (y & m2)
}

#[inline]
fn shift_left<F: TowerField>(x: M128) -> M128 {
	let tower_level = F::TOWER_LEVEL;
	seq!(TOWER_LEVEL in 0..=5 {
		if tower_level == TOWER_LEVEL {
			return unsafe { vshlq_n_u64(x.into(), 1 << TOWER_LEVEL).into() };
		}
	});
	if tower_level == 6 {
		return unsafe { vcombine_u64(vcreate_u64(0), vget_low_u64(x.into())).into() };
	}
	panic!("Unsupported tower level {tower_level}");
}

#[inline]
fn shift_right<F: TowerField>(x: M128) -> M128 {
	let tower_level = F::TOWER_LEVEL;
	seq!(TOWER_LEVEL in 0..=5 {
		if tower_level == TOWER_LEVEL {
			return unsafe { vshrq_n_u64(x.into(), 1 << TOWER_LEVEL).into() };
		}
	});
	if tower_level == 6 {
		return unsafe { vcombine_u64(vget_high_u64(x.into()), vcreate_u64(0)).into() };
	}
	panic!("Unsupported tower level {tower_level}");
}

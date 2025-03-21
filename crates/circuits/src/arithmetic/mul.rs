// Copyright 2025 Irreducible Inc.

//! Multiplication based on exponentiation.
//!
//! The core idea of this method is to verify the equality $a \cdot b = c$
//! by checking if $(g^a)^b = g^{clow} \cdot (g^{2^{len(clow)}})^{chigh}$,
//! where exponentiation proofs can be efficiently verified using the GKR exponentiation protocol.
//!
//! You can read more information in [Integer Multiplication in Binius](https://www.irreducible.com/posts/integer-multiplication-in-binius).

use anyhow::Error;
use binius_core::oracle::OracleId;
use binius_field::{
	as_packed_field::PackedType,
	packed::{get_packed_slice, set_packed_slice},
	BinaryField, BinaryField1b, Field, TowerField,
};
use binius_macros::arith_expr;
use binius_maybe_rayon::iter::{
	IndexedParallelIterator, IntoParallelRefMutIterator, ParallelIterator,
};
use binius_utils::bail;

use crate::builder::{
	types::{F, U},
	ConstraintSystemBuilder,
};

pub fn mul<FExpBase>(
	builder: &mut ConstraintSystemBuilder,
	name: impl ToString,
	xin_bits: Vec<OracleId>,
	yin_bits: Vec<OracleId>,
) -> Result<Vec<OracleId>, anyhow::Error>
where
	FExpBase: TowerField,
	F: From<FExpBase>,
{
	let name = name.to_string();

	let log_rows = builder.log_rows([xin_bits.clone(), yin_bits.clone()].into_iter().flatten())?;

	// $g^x$
	let xin_exp_result_id =
		builder.add_committed(format!("{} xin_exp_result", name), log_rows, FExpBase::TOWER_LEVEL);

	// $(g^x)^y$
	let yin_exp_result_id =
		builder.add_committed(format!("{} yin_exp_result", name), log_rows, FExpBase::TOWER_LEVEL);

	// $g^{clow}$
	let cout_low_exp_result_id = builder.add_committed(
		format!("{} cout_low_exp_result", name),
		log_rows,
		FExpBase::TOWER_LEVEL,
	);

	// $(g^{2^{len(clow)}})^{chigh}$
	let cout_high_exp_result_id = builder.add_committed(
		format!("{} cout_high_exp_result", name),
		log_rows,
		FExpBase::TOWER_LEVEL,
	);

	let result_bits = xin_bits.len() + yin_bits.len();

	if result_bits > FExpBase::N_BITS {
		bail!(anyhow::anyhow!("FExpBase to small"));
	}

	let cout_bits = (0..result_bits)
		.map(|i| {
			builder.add_committed(
				format!("{} bit of {}", i, name),
				log_rows,
				BinaryField1b::TOWER_LEVEL,
			)
		})
		.collect::<Vec<_>>();

	if let Some(witness) = builder.witness() {
		let xin_columns = xin_bits
			.iter()
			.map(|&id| witness.get::<BinaryField1b>(id).map(|x| x.packed()))
			.collect::<Result<Vec<_>, Error>>()?;

		let yin_columns = yin_bits
			.iter()
			.map(|&id| witness.get::<BinaryField1b>(id).map(|x| x.packed()))
			.collect::<Result<Vec<_>, Error>>()?;

		let result = columns_to_numbers(&xin_columns)
			.into_iter()
			.zip(columns_to_numbers(&yin_columns))
			.map(|(x, y)| x * y)
			.collect::<Vec<_>>();

		let mut cout_columns = cout_bits
			.iter()
			.map(|&id| witness.new_column::<BinaryField1b>(id))
			.collect::<Vec<_>>();

		let mut cout_columns_u8 = cout_columns
			.iter_mut()
			.map(|column| column.packed())
			.collect::<Vec<_>>();

		numbers_to_columns(&result, &mut cout_columns_u8);
	}

	// Handling special case when $x == 0$ $y == 0$ $c == 2^{2 \cdot n} -1$
	builder.assert_zero(
		name.clone(),
		[xin_bits[0], yin_bits[0], cout_bits[0]],
		arith_expr!([xin, yin, cout] = xin * yin - cout).convert_field(),
	);

	// $(g^x)^y = g^{clow} * (g^{2^{len(clow)}})^{chigh}$
	builder.assert_zero(
		name,
		[
			yin_exp_result_id,
			cout_low_exp_result_id,
			cout_high_exp_result_id,
		],
		arith_expr!([yin, low, high] = low * high - yin).convert_field(),
	);

	let (cout_low_bits, cout_high_bits) = cout_bits.split_at(cout_bits.len() / 2);

	builder.add_static_exp(
		xin_bits,
		xin_exp_result_id,
		FExpBase::MULTIPLICATIVE_GENERATOR.into(),
		FExpBase::TOWER_LEVEL,
	);
	builder.add_dynamic_exp(yin_bits, yin_exp_result_id, xin_exp_result_id);
	builder.add_static_exp(
		cout_low_bits.to_vec(),
		cout_low_exp_result_id,
		FExpBase::MULTIPLICATIVE_GENERATOR.into(),
		FExpBase::TOWER_LEVEL,
	);
	builder.add_static_exp(
		cout_high_bits.to_vec(),
		cout_high_exp_result_id,
		exp_pow2(FExpBase::MULTIPLICATIVE_GENERATOR, cout_low_bits.len()).into(),
		FExpBase::TOWER_LEVEL,
	);

	Ok(cout_bits)
}

fn exp_pow2<F: BinaryField>(mut g: F, log_exp: usize) -> F {
	for _ in 0..log_exp {
		g *= g
	}
	g
}

fn columns_to_numbers(columns: &[&[PackedType<U, BinaryField1b>]]) -> Vec<u128> {
	let width = PackedType::<U, BinaryField1b>::WIDTH;
	let mut numbers: Vec<u128> = vec![0; columns.first().map(|c| c.len() * width).unwrap_or(0)];

	for (bit, column) in columns.iter().enumerate() {
		numbers.par_iter_mut().enumerate().for_each(|(i, number)| {
			if get_packed_slice(column, i) == BinaryField1b::ONE {
				*number |= 1 << bit;
			}
		});
	}
	numbers
}

fn numbers_to_columns(numbers: &[u128], columns: &mut [&mut [PackedType<U, BinaryField1b>]]) {
	columns
		.par_iter_mut()
		.enumerate()
		.for_each(|(bit, column)| {
			for (i, number) in numbers.iter().enumerate() {
				if (number >> bit) & 1 == 1 {
					set_packed_slice(column, i, BinaryField1b::ONE);
				}
			}
		});
}

#[cfg(test)]
mod tests {
	use binius_core::{
		constraint_system::{self},
		fiat_shamir::HasherChallenger,
		tower::CanonicalTowerFamily,
	};
	use binius_field::{BinaryField1b, BinaryField8b};
	use binius_hal::make_portable_backend;
	use binius_hash::groestl::{Groestl256, Groestl256ByteCompression};
	use binius_math::DefaultEvaluationDomainFactory;

	use super::mul;
	use crate::{
		builder::{types::U, ConstraintSystemBuilder},
		unconstrained::unconstrained,
	};

	#[test]
	fn test_mul() {
		let allocator = bumpalo::Bump::new();
		let mut builder = ConstraintSystemBuilder::new_with_witness(&allocator);

		let log_n_muls = 9;

		let in_a = (0..2)
			.map(|i| {
				unconstrained::<BinaryField1b>(&mut builder, format!("in_a_{}", i), log_n_muls)
					.unwrap()
			})
			.collect::<Vec<_>>();
		let in_b = (0..2)
			.map(|i| {
				unconstrained::<BinaryField1b>(&mut builder, format!("in_b_{}", i), log_n_muls)
					.unwrap()
			})
			.collect::<Vec<_>>();

		mul::<BinaryField8b>(&mut builder, "test", in_a, in_b).unwrap();

		let witness = builder
			.take_witness()
			.expect("builder created with witness");

		let constraint_system = builder.build().unwrap();

		let domain_factory = DefaultEvaluationDomainFactory::default();
		let backend = make_portable_backend();

		let proof = constraint_system::prove::<
			U,
			CanonicalTowerFamily,
			_,
			Groestl256,
			Groestl256ByteCompression,
			HasherChallenger<Groestl256>,
			_,
		>(&constraint_system, 1, 10, &[], witness, &domain_factory, &backend)
		.unwrap();

		constraint_system::verify::<
			U,
			CanonicalTowerFamily,
			Groestl256,
			Groestl256ByteCompression,
			HasherChallenger<Groestl256>,
		>(&constraint_system, 1, 10, &[], proof)
		.unwrap();
	}
}

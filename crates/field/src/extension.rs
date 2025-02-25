// Copyright 2023-2025 Irreducible Inc.

use std::{
	iter,
	ops::{Add, AddAssign, Mul, MulAssign, Sub, SubAssign},
};

use super::{error::Error, Field};

pub trait ExtensionField<F: Field>:
	Field
	+ From<F>
	+ TryInto<F>
	+ Add<F, Output = Self>
	+ Sub<F, Output = Self>
	+ Mul<F, Output = Self>
	+ AddAssign<F>
	+ SubAssign<F>
	+ MulAssign<F>
{
	/// Base-2 logarithm of the extension degree.
	const LOG_DEGREE: usize;

	/// Extension degree.
	///
	/// `DEGREE` is guaranteed to equal `2^LOG_DEGREE`.
	const DEGREE: usize = 1 << Self::LOG_DEGREE;

	/// For `0 <= i < DEGREE`, returns `i`-th basis field element.
	fn basis(i: usize) -> Result<Self, Error>;

	/// Create an extension field element from a slice of base field elements in order
	/// consistent with `basis(i)` return values.
	/// Potentially faster than taking an inner product with a vector of basis elements.
	fn from_bases(base_elems: &[F]) -> Result<Self, Error> {
		Self::from_bases_sparse(base_elems, 0)
	}

	/// A specialized version of `from_bases` which assumes that only base field
	/// elements with indices dividing `2^log_stride` can be nonzero.
	///
	/// `base_elems` should have length at most `ceil(DEGREE / 2^LOG_STRIDE)`. Note that
	/// [`ExtensionField::from_bases`] is a special case of `from_bases_sparse` with `log_stride = 0`.
	fn from_bases_sparse(base_elems: &[F], log_stride: usize) -> Result<Self, Error>;

	/// Iterator over base field elements.
	fn iter_bases(&self) -> impl Iterator<Item = F>;

	/// Convert into an iterator over base field elements.
	fn into_iter_bases(self) -> impl Iterator<Item = F>;
}

impl<F: Field> ExtensionField<F> for F {
	const LOG_DEGREE: usize = 0;

	fn basis(i: usize) -> Result<Self, Error> {
		if i != 0 {
			return Err(Error::ExtensionDegreeMismatch);
		}
		Ok(Self::ONE)
	}

	fn from_bases_sparse(base_elems: &[F], log_stride: usize) -> Result<Self, Error> {
		if log_stride != 0 {
			return Err(Error::ExtensionDegreeMismatch);
		}

		match base_elems.len() {
			0 => Ok(F::ZERO),
			1 => Ok(base_elems[0]),
			_ => Err(Error::ExtensionDegreeMismatch),
		}
	}

	fn iter_bases(&self) -> impl Iterator<Item = F> {
		iter::once(*self)
	}

	fn into_iter_bases(self) -> impl Iterator<Item = F> {
		iter::once(self)
	}
}

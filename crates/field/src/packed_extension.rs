// Copyright 2023-2025 Irreducible Inc.

use crate::{
	ExtensionField, Field, PackedField,
	as_packed_field::PackScalar,
	underlier::{Divisible, WithUnderlier},
};

/// A [`PackedField`] that can be safely cast to indexable slices of scalars.
///
/// Not all packed fields can index individual scalar elements. Notably, packed fields of
/// $\mathbb{F}_2$ elements can pack multiple scalars into a single byte.
///
///
/// # Safety
///
/// In order for the above relation to be guaranteed, the memory representation of a slice of
/// `PackedExtensionIndexable` elements must be the same as a slice of the underlying scalar
/// elements, differing only in the slice lengths.
pub unsafe trait PackedFieldIndexable: PackedField {
	fn unpack_scalars(packed: &[Self]) -> &[Self::Scalar];
	fn unpack_scalars_mut(packed: &mut [Self]) -> &mut [Self::Scalar];
}

unsafe impl<S, P> PackedFieldIndexable for P
where
	S: Field,
	P: PackedDivisible<S, Scalar = S>,
{
	fn unpack_scalars(packed: &[Self]) -> &[Self::Scalar] {
		P::divide(packed)
	}

	fn unpack_scalars_mut(packed: &mut [Self]) -> &mut [Self::Scalar] {
		P::divide_mut(packed)
	}
}

/// Check if `P` implements `PackedFieldIndexable`.
/// This functions gets optimized out by the compiler so if it is used in a generic context
/// as an `if` condition, the non-meaningful branch will be optimized out.
#[inline(always)]
#[allow(clippy::redundant_clone)]
pub fn is_packed_field_indexable<P: PackedField>() -> bool {
	// Use a hack that array of copyable elements won't call clone when the array is cloned.

	struct X<T> {
		cloned: bool,
		_pd: std::marker::PhantomData<T>,
	}

	impl<T> Clone for X<T> {
		fn clone(&self) -> Self {
			Self {
				cloned: true,
				_pd: std::marker::PhantomData,
			}
		}
	}

	impl<T: PackedFieldIndexable> Copy for X<T> {}

	let arr = [X::<P> {
		cloned: false,
		_pd: std::marker::PhantomData,
	}];
	let cloned = arr.clone();

	!cloned[0].cloned
}

#[inline(always)]
pub fn unpack_if_possible<P: PackedField, R>(
	slice: &[P],
	unpacked_fn: impl FnOnce(&[P::Scalar]) -> R,
	fallback_fn: impl FnOnce(&[P]) -> R,
) -> R {
	if is_packed_field_indexable::<P>() {
		let unpacked = unsafe {
			std::slice::from_raw_parts(
				slice.as_ptr() as *const P::Scalar,
				slice.len() << P::LOG_WIDTH,
			)
		};
		unpacked_fn(unpacked)
	} else {
		fallback_fn(slice)
	}
}

#[inline(always)]
pub fn unpack_if_possible_mut<P: PackedField, R>(
	slice: &mut [P],
	unpacked_fn: impl FnOnce(&mut [P::Scalar]) -> R,
	fallback_fn: impl FnOnce(&mut [P]) -> R,
) -> R {
	if is_packed_field_indexable::<P>() {
		let unpacked = unsafe {
			std::slice::from_raw_parts_mut(
				slice.as_mut_ptr() as *mut P::Scalar,
				slice.len() << P::LOG_WIDTH,
			)
		};
		unpacked_fn(unpacked)
	} else {
		fallback_fn(slice)
	}
}

/// Trait represents a relationship between a packed struct of field elements and a packed struct
/// of elements from an extension field.
///
/// This trait guarantees that one packed type has the same
/// memory representation as the other, differing only in the scalar type and preserving the order
/// of smaller elements.
///
/// This trait relation guarantees that the following iterators yield the same sequence of scalar
/// elements:
///
/// ```
/// use binius_field::{ExtensionField, PackedExtension, PackedField, Field};
///
/// fn ext_then_bases<'a, F, PE>(packed: &'a PE) -> impl Iterator<Item=F> + 'a
///     where
///         PE: PackedField<Scalar: ExtensionField<F>>,
///         F: Field,
/// {
///     packed.iter().flat_map(|ext| ext.into_iter_bases())
/// }
///
/// fn cast_then_iter<'a, F, PE>(packed: &'a PE) -> impl Iterator<Item=F> + 'a
///     where
///         PE: PackedExtension<F>,
///         F: Field,
/// {
///     PE::cast_base_ref(packed).into_iter()
/// }
/// ```
///
/// # Safety
///
/// In order for the above relation to be guaranteed, the memory representation of
/// `PackedExtensionField` element must be the same as a slice of the underlying `PackedField`
/// element.
pub trait PackedExtension<FS: Field>: PackedField<Scalar: ExtensionField<FS>> {
	type PackedSubfield: PackedField<Scalar = FS>;

	fn cast_bases(packed: &[Self]) -> &[Self::PackedSubfield];
	fn cast_bases_mut(packed: &mut [Self]) -> &mut [Self::PackedSubfield];

	fn cast_exts(packed: &[Self::PackedSubfield]) -> &[Self];
	fn cast_exts_mut(packed: &mut [Self::PackedSubfield]) -> &mut [Self];

	fn cast_base(self) -> Self::PackedSubfield;
	fn cast_base_ref(&self) -> &Self::PackedSubfield;
	fn cast_base_mut(&mut self) -> &mut Self::PackedSubfield;

	fn cast_ext(base: Self::PackedSubfield) -> Self;
	fn cast_ext_ref(base: &Self::PackedSubfield) -> &Self;
	fn cast_ext_mut(base: &mut Self::PackedSubfield) -> &mut Self;

	#[inline(always)]
	fn cast_base_arr<const N: usize>(packed: [Self; N]) -> [Self::PackedSubfield; N] {
		packed.map(Self::cast_base)
	}

	#[inline(always)]
	fn cast_base_arr_ref<const N: usize>(packed: &[Self; N]) -> &[Self::PackedSubfield; N] {
		Self::cast_bases(packed)
			.try_into()
			.expect("array has size N")
	}

	#[inline(always)]
	fn cast_base_arr_mut<const N: usize>(packed: &mut [Self; N]) -> &mut [Self::PackedSubfield; N] {
		Self::cast_bases_mut(packed)
			.try_into()
			.expect("array has size N")
	}

	#[inline(always)]
	fn cast_ext_arr<const N: usize>(packed: [Self::PackedSubfield; N]) -> [Self; N] {
		packed.map(Self::cast_ext)
	}

	#[inline(always)]
	fn cast_ext_arr_ref<const N: usize>(packed: &[Self::PackedSubfield; N]) -> &[Self; N] {
		Self::cast_exts(packed)
			.try_into()
			.expect("array has size N")
	}

	#[inline(always)]
	fn cast_ext_arr_mut<const N: usize>(packed: &mut [Self::PackedSubfield; N]) -> &mut [Self; N] {
		Self::cast_exts_mut(packed)
			.try_into()
			.expect("array has size N")
	}
}

impl<PT, FS> PackedExtension<FS> for PT
where
	FS: Field,
	PT: PackedField<Scalar: ExtensionField<FS>> + WithUnderlier<Underlier: PackScalar<FS>>,
{
	type PackedSubfield = <PT::Underlier as PackScalar<FS>>::Packed;

	fn cast_bases(packed: &[Self]) -> &[Self::PackedSubfield] {
		Self::PackedSubfield::from_underliers_ref(Self::to_underliers_ref(packed))
	}

	fn cast_bases_mut(packed: &mut [Self]) -> &mut [Self::PackedSubfield] {
		Self::PackedSubfield::from_underliers_ref_mut(Self::to_underliers_ref_mut(packed))
	}

	fn cast_exts(base: &[Self::PackedSubfield]) -> &[Self] {
		Self::from_underliers_ref(Self::PackedSubfield::to_underliers_ref(base))
	}

	fn cast_exts_mut(base: &mut [Self::PackedSubfield]) -> &mut [Self] {
		Self::from_underliers_ref_mut(Self::PackedSubfield::to_underliers_ref_mut(base))
	}

	fn cast_base(self) -> Self::PackedSubfield {
		Self::PackedSubfield::from_underlier(self.to_underlier())
	}

	fn cast_base_ref(&self) -> &Self::PackedSubfield {
		Self::PackedSubfield::from_underlier_ref(self.to_underlier_ref())
	}

	fn cast_base_mut(&mut self) -> &mut Self::PackedSubfield {
		Self::PackedSubfield::from_underlier_ref_mut(self.to_underlier_ref_mut())
	}

	fn cast_ext(base: Self::PackedSubfield) -> Self {
		Self::from_underlier(base.to_underlier())
	}

	fn cast_ext_ref(base: &Self::PackedSubfield) -> &Self {
		Self::from_underlier_ref(base.to_underlier_ref())
	}

	fn cast_ext_mut(base: &mut Self::PackedSubfield) -> &mut Self {
		Self::from_underlier_ref_mut(base.to_underlier_ref_mut())
	}
}

/// Convenient type alias that returns the packed field type for the scalar field `F` and packed
/// extension `P`.
pub type PackedSubfield<P, F> = <P as PackedExtension<F>>::PackedSubfield;

/// Recast a packed field from one subfield of a packed extension to another.
pub fn recast_packed<P, FSub1, FSub2>(elem: PackedSubfield<P, FSub1>) -> PackedSubfield<P, FSub2>
where
	P: PackedField + PackedExtension<FSub1> + PackedExtension<FSub2>,
	P::Scalar: ExtensionField<FSub1> + ExtensionField<FSub2>,
	FSub1: Field,
	FSub2: Field,
{
	<P as PackedExtension<FSub2>>::cast_base(<P as PackedExtension<FSub1>>::cast_ext(elem))
}

/// Recast a slice of packed field elements from one subfield of a packed extension to another.
pub fn recast_packed_slice<P, FSub1, FSub2>(
	elems: &[PackedSubfield<P, FSub1>],
) -> &[PackedSubfield<P, FSub2>]
where
	P: PackedField + PackedExtension<FSub1> + PackedExtension<FSub2>,
	P::Scalar: ExtensionField<FSub1> + ExtensionField<FSub2>,
	FSub1: Field,
	FSub2: Field,
{
	<P as PackedExtension<FSub2>>::cast_bases(<P as PackedExtension<FSub1>>::cast_exts(elems))
}

/// Recast a mutable slice of packed field elements from one subfield of a packed extension to
/// another.
pub fn recast_packed_mut<P, FSub1, FSub2>(
	elems: &mut [PackedSubfield<P, FSub1>],
) -> &mut [PackedSubfield<P, FSub2>]
where
	P: PackedField + PackedExtension<FSub1> + PackedExtension<FSub2>,
	P::Scalar: ExtensionField<FSub1> + ExtensionField<FSub2>,
	FSub1: Field,
	FSub2: Field,
{
	<P as PackedExtension<FSub2>>::cast_bases_mut(<P as PackedExtension<FSub1>>::cast_exts_mut(
		elems,
	))
}

/// This trait is a shorthand for the case `PackedExtension<P::Scalar, PackedSubfield = P>` which is
/// a quite common case in our codebase.
pub trait RepackedExtension<P: PackedField>:
	PackedField<Scalar: ExtensionField<P::Scalar>> + PackedExtension<P::Scalar, PackedSubfield = P>
{
}

impl<PT1, PT2> RepackedExtension<PT1> for PT2
where
	PT1: PackedField,
	PT2: PackedExtension<PT1::Scalar, PackedSubfield = PT1, Scalar: ExtensionField<PT1::Scalar>>,
{
}

/// This trait adds shortcut methods for the case `PackedExtension<F, PackedSubfield:
/// PackedFieldIndexable>` which is a quite common case in our codebase.
pub trait PackedExtensionIndexable<F: Field>:
	PackedExtension<F, PackedSubfield: PackedFieldIndexable> + PackedField<Scalar: ExtensionField<F>>
{
	fn unpack_base_scalars(packed: &[Self]) -> &[F] {
		Self::PackedSubfield::unpack_scalars(Self::cast_bases(packed))
	}

	fn unpack_base_scalars_mut(packed: &mut [Self]) -> &mut [F] {
		Self::PackedSubfield::unpack_scalars_mut(Self::cast_bases_mut(packed))
	}
}

impl<F, PT> PackedExtensionIndexable<F> for PT
where
	F: Field,
	PT: PackedExtension<F, PackedSubfield: PackedFieldIndexable>,
{
}

/// Trait represents a relationship between a packed struct of field elements and a smaller packed
/// struct the same field elements.
///
/// This trait can be used to safely cast memory slices from larger packed fields to smaller ones.
///
/// # Safety
///
/// In order for the above relation to be guaranteed, the memory representation of a slice of
/// `PackedDivisible` elements must be the same as a slice of the underlying `PackedField`
/// elements, differing only in the slice lengths.
pub unsafe trait PackedDivisible<P>: PackedField
where
	P: PackedField<Scalar = Self::Scalar>,
{
	fn divide(packed: &[Self]) -> &[P];
	fn divide_mut(packed: &mut [Self]) -> &mut [P];
}

unsafe impl<PT1, PT2> PackedDivisible<PT2> for PT1
where
	PT2: PackedField + WithUnderlier,
	PT1: PackedField<Scalar = PT2::Scalar> + WithUnderlier<Underlier: Divisible<PT2::Underlier>>,
{
	fn divide(packed: &[Self]) -> &[PT2] {
		let underliers = PT1::to_underliers_ref(packed);
		let underliers: &[PT2::Underlier] = PT1::Underlier::split_slice(underliers);
		PT2::from_underliers_ref(underliers)
	}

	fn divide_mut(packed: &mut [Self]) -> &mut [PT2] {
		let underliers = PT1::to_underliers_ref_mut(packed);
		let underliers: &mut [PT2::Underlier] = PT1::Underlier::split_slice_mut(underliers);
		PT2::from_underliers_ref_mut(underliers)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{PackedBinaryField2x8b, PackedBinaryField8x2b};

	#[test]
	fn test_unpack_if_possible() {
		let slice = [PackedBinaryField2x8b::zero(); 4];

		let len = unpack_if_possible(&slice, |slice| slice.len(), |slice| slice.len());
		assert_eq!(len, 8);

		let slice = [PackedBinaryField8x2b::zero(); 4];
		let len = unpack_if_possible(&slice, |slice| slice.len(), |slice| slice.len());
		assert_eq!(len, 4);
	}

	#[test]
	fn test_unpack_if_possible_mut() {
		let mut slice = [PackedBinaryField2x8b::zero(); 4];

		let len = unpack_if_possible_mut(&mut slice, |slice| slice.len(), |slice| slice.len());
		assert_eq!(len, 8);

		let mut slice = [PackedBinaryField8x2b::zero(); 4];
		let len = unpack_if_possible_mut(&mut slice, |slice| slice.len(), |slice| slice.len());
		assert_eq!(len, 4);
	}
}

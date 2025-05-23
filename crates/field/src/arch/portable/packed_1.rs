// Copyright 2024-2025 Irreducible Inc.

use super::packed::{PackedPrimitiveType, impl_broadcast, impl_ops_for_zero_height};
use crate::{
	BinaryField1b, arch::PairwiseStrategy, arithmetic_traits::impl_transformation_with_strategy,
	underlier::U1,
};

// Define 1 bit packed field types
pub type PackedBinaryField1x1b = PackedPrimitiveType<U1, BinaryField1b>;

// Define broadcast
impl_broadcast!(U1, BinaryField1b);

// Define operations for height 0
impl_ops_for_zero_height!(PackedBinaryField1x1b);

// Define linear transformations
impl_transformation_with_strategy!(PackedBinaryField1x1b, PairwiseStrategy);

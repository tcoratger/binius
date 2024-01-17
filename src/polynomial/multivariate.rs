// Copyright 2023 Ulvetanna Inc.

use super::{
	error::Error, multilinear_extension::MultilinearExtension, multilinear_query::MultilinearQuery,
	MultilinearPoly,
};
use crate::field::PackedField;
use std::{borrow::Borrow, fmt::Debug, marker::PhantomData, sync::Arc};

pub trait MultivariatePoly<F>: std::fmt::Debug {
	// The number of variables.
	fn n_vars(&self) -> usize;

	/// Total degree of the polynomial.
	fn degree(&self) -> usize;

	/// Evaluate the polynomial at a point in the extension field.
	fn evaluate(&self, query: &[F]) -> Result<F, Error>;
}

/// A multivariate polynomial that defines a composition of `MultilinearComposite`.
pub trait CompositionPoly<P>: std::fmt::Debug + Send + Sync
where
	P: PackedField,
{
	// The number of variables.
	fn n_vars(&self) -> usize;

	/// Total degree of the polynomial.
	fn degree(&self) -> usize;

	/// Evaluate the polynomial at packed evaluation points.
	fn evaluate(&self, query: &[P]) -> Result<P, Error>;
}

#[derive(Debug)]
pub struct IdentityCompositionPoly;

impl<P: PackedField> CompositionPoly<P> for IdentityCompositionPoly {
	fn n_vars(&self) -> usize {
		1
	}

	fn degree(&self) -> usize {
		1
	}

	fn evaluate(&self, query: &[P]) -> Result<P, Error> {
		if query.len() != 1 {
			return Err(Error::IncorrectQuerySize { expected: 1 });
		}
		Ok(query[0])
	}
}

/// A polynomial defined as the composition of several multilinear polynomials.
///
/// A $\mu$-variate multilinear composite polynomial $p(X_0, ..., X_{\mu})$ is defined as
///
/// $$
/// g(f_0(X_0, ..., X_{\mu}), ..., f_{k-1}(X_0, ..., X_{\mu}))
/// $$
///
/// where $g(Y_0, ..., Y_{k-1})$ is a $k$-variate polynomial and $f_0, ..., f_k$ are all multilinear
/// in $\mu$ variables.
///
/// TODO: Document type parameters. The BM: Borrow<M> type parameter is necessary so that we can
/// handle the case of a MultilinearComposite that contains boxed trait objects as well as the case
/// where it directly holds some implementation of MultilinearPoly.
#[derive(Debug)]
pub struct MultilinearComposite<P, M, BM>
where
	P: PackedField,
	M: MultilinearPoly<P> + ?Sized,
	BM: Borrow<M>,
{
	// TODO: Consider whether to define this struct as generic over the composition function
	pub composition: Arc<dyn CompositionPoly<P>>,
	n_vars: usize,
	// The multilinear polynomials. The length of the vector matches `composition.n_vars()`.
	pub multilinears: Vec<BM>,
	_m_marker: PhantomData<M>,
}

impl<P, M, BM> Clone for MultilinearComposite<P, M, BM>
where
	P: PackedField,
	M: MultilinearPoly<P> + ?Sized,
	BM: Borrow<M> + Clone,
{
	fn clone(&self) -> Self {
		Self {
			composition: self.composition.clone(),
			n_vars: self.n_vars,
			multilinears: self.multilinears.clone(),
			_m_marker: PhantomData,
		}
	}
}

impl<P, M, BM> MultilinearComposite<P, M, BM>
where
	P: PackedField + Debug,
	M: MultilinearPoly<P> + ?Sized,
	BM: Borrow<M>,
{
	pub fn new(
		n_vars: usize,
		composition: Arc<dyn CompositionPoly<P>>,
		multilinears: Vec<BM>,
	) -> Result<Self, Error> {
		if composition.n_vars() != multilinears.len() {
			let err_str = format!(
				"Number of variables in composition {} does not match length of multilinears {}",
				composition.n_vars(),
				multilinears.len()
			);
			return Err(Error::MultilinearCompositeValidation(err_str));
		}
		for multilin in multilinears.iter().map(Borrow::borrow) {
			if multilin.n_vars() != n_vars {
				let err_str = format!(
					"Number of variables in multilinear {} does not match n_vars {}",
					multilin.n_vars(),
					n_vars
				);
				return Err(Error::MultilinearCompositeValidation(err_str));
			}
		}
		Ok(Self {
			n_vars,
			composition,
			multilinears,
			_m_marker: PhantomData,
		})
	}

	pub fn iter_multilinear_polys(&self) -> impl Iterator<Item = &M> {
		self.multilinears.iter().map(Borrow::borrow)
	}

	pub fn evaluate_partial_low(
		&self,
		query: &[P::Scalar],
	) -> Result<
		MultilinearComposite<P, MultilinearExtension<'static, P>, MultilinearExtension<'static, P>>,
		Error,
	> {
		let new_multilinears = self
			.iter_multilinear_polys()
			.map(|multilin| {
				multilin.evaluate_partial_low(&MultilinearQuery::with_full_query(query)?)
			})
			.collect::<Result<Vec<_>, _>>()?;
		Ok(MultilinearComposite {
			composition: self.composition.clone(),
			n_vars: self.n_vars - query.len(),
			multilinears: new_multilinears,
			_m_marker: PhantomData,
		})
	}

	pub fn degree(&self) -> usize {
		self.composition.degree()
	}

	pub fn n_vars(&self) -> usize {
		self.n_vars
	}

	pub fn n_multilinears(&self) -> usize {
		self.composition.n_vars()
	}

	pub fn from_multilinear(poly: BM) -> Self {
		MultilinearComposite {
			composition: Arc::new(IdentityCompositionPoly),
			n_vars: poly.borrow().n_vars(),
			multilinears: vec![poly],
			_m_marker: PhantomData,
		}
	}
}

// Copyright 2023 Ulvetanna Inc.

use super::error::{Error, VerificationError};
use crate::{
	linear_code::LinearCode,
	merkle_tree::{MerkleTreeVCS, VectorCommitScheme},
	poly_commit::PolyCommitScheme,
	polynomial::{
		multilinear_query::MultilinearQuery, Error as PolynomialError, MultilinearExtension,
	},
	reed_solomon::reed_solomon::ReedSolomonCode,
};
use binius_field::{
	packed::{get_packed_slice, iter_packed_slice},
	square_transpose, transpose_scalars,
	util::inner_product_unchecked,
	BinaryField, BinaryField8b, ExtensionField, Field, PackedExtensionField, PackedField,
	PackedFieldIndexable,
};
use binius_hash::{hash, GroestlDigest, GroestlDigestCompression, GroestlHasher, Hasher};
use p3_challenger::{CanObserve, CanSample, CanSampleBits};
use p3_matrix::{dense::RowMajorMatrix, MatrixRowSlices};
use p3_util::{log2_ceil_usize, log2_strict_usize};
use rayon::prelude::*;
use std::{iter::repeat_with, marker::PhantomData, mem};
use tracing::instrument;

/// Creates a new multilinear from a batch of multilinears and a mixing challenge
///
/// REQUIRES:
///     All inputted multilinear polynomials have $\mu := \text{n_vars}$ variables
///     t_primes.len() == mixing_coeffs.len()
/// ENSURES:
///     Given a batch of $m$ multilinear polynomials $t_i$'s, and $n$ mixing coeffs $c_i$,
///     this function computes the multilinear polynomial $t$ such that
///     $\forall v \in \{0, 1\}^{\mu}$, $t(v) = \sum_{i=0}^{n-1} c_i * t_i(v)$
fn mix_t_primes<F, P>(
	n_vars: usize,
	t_primes: &[MultilinearExtension<'_, P>],
	mixing_coeffs: &[F],
) -> Result<MultilinearExtension<'static, P>, Error>
where
	F: Field,
	P: PackedField<Scalar = F>,
{
	for t_prime_i in t_primes {
		if t_prime_i.n_vars() != n_vars {
			return Err(Error::IncorrectPolynomialSize { expected: n_vars });
		}
	}

	let mixed_evals = (0..(1 << n_vars) / P::WIDTH)
		.into_par_iter()
		.map(|i| {
			t_primes
				.iter()
				.map(|t_prime| t_prime.evals()[i])
				.zip(mixing_coeffs.iter().copied())
				.map(|(t_prime_i, coeff)| t_prime_i * coeff)
				.sum()
		})
		.collect::<Vec<_>>();

	let mixed_t_prime = MultilinearExtension::from_values(mixed_evals)?;
	Ok(mixed_t_prime)
}

/// Evaluation proof data for the `TensorPCS` polynomial commitment scheme.
///
/// # Type Parameters
///
/// * `PI`: The packed intermediate field type.
/// * `PE`: The packed extension field type.
/// * `VCSProof`: The vector commitment scheme proof type.
#[derive(Debug)]
pub struct Proof<'a, PI, PE, VCSProof>
where
	PE: PackedField,
{
	/// Number of distinct multilinear polynomials in the batch opening proof
	pub n_polys: usize,
	/// Represents a mixing of individual polynomial t_primes
	///
	/// Let $n$ denote n_polys. Define $l = \lceil\log_2(n)\rceil$.
	/// Let $\alpha_0, \ldots, \alpha_{l-1}$ be the sampled mixing challenges.
	/// Then $c := \otimes_{i=0}^{l-1} (1 - \alpha_i, \alpha_i)$ are the $2^l$ mixing coefficients,
	/// denoting the $i$-th coefficient by $c_i$.
	/// Let $t'_i$ denote the $t'$ for the $i$-th polynomial in the batch opening proof.
	/// This value represents the multilinear polynomial such that $\forall v \in \{0, 1\}^{\mu}$,
	/// $v \rightarrow \sum_{i=0}^{n-1} c_i * t'_i(v)$
	pub mixed_t_prime: MultilinearExtension<'a, PE>,
	/// Opening proofs for chosen columns of the encoded matrices
	///
	/// Let $j_1, \ldots, j_k$ be the indices of the columns that are opened.
	/// The ith element is a tuple of:
	/// * A vector (size=n_polys) of the $j_i$th columns (one from each polynomial's encoded matrix)
	/// * A proof that these columns are consistent with the vector commitment
	pub vcs_proofs: Vec<(Vec<Vec<PI>>, VCSProof)>,
}

/// The multilinear polynomial commitment scheme specified in [DP23].
///
/// # Type Parameters
///
/// * `P`: The base field type of committed elements.
/// * `PA`: The field type of the encoding alphabet.
/// * `PI`: The intermediate field type that base field elements are packed into.
/// * `PE`: The extension field type used for cryptographic challenges.
///
/// [DP23]: https://eprint.iacr.org/2023/630
#[derive(Debug, Copy, Clone)]
pub struct TensorPCS<P, PA, PI, PE, LC, H, VCS>
where
	P: PackedField,
	PA: PackedField,
	PI: PackedField,
	PE: PackedField,
	LC: LinearCode<P = PA>,
	H: Hasher<PI>,
	VCS: VectorCommitScheme<H::Digest>,
{
	log_rows: usize,
	n_test_queries: usize,
	code: LC,
	vcs: VCS,
	_p_marker: PhantomData<P>,
	_pi_marker: PhantomData<PI>,
	_h_marker: PhantomData<H>,
	_ext_marker: PhantomData<PE>,
}

type GroestlMerkleTreeVCS = MerkleTreeVCS<
	GroestlDigest,
	GroestlDigest,
	GroestlHasher<GroestlDigest>,
	GroestlDigestCompression,
>;

impl<P, PA, PI, PE, LC> TensorPCS<P, PA, PI, PE, LC, GroestlHasher<PI>, GroestlMerkleTreeVCS>
where
	P: PackedField,
	PA: PackedField,
	PI: PackedField + PackedExtensionField<BinaryField8b> + Sync,
	PI::Scalar: ExtensionField<P::Scalar> + ExtensionField<BinaryField8b>,
	PE: PackedField,
	PE::Scalar: ExtensionField<P::Scalar> + BinaryField,
	LC: LinearCode<P = PA>,
{
	pub fn new_using_groestl_merkle_tree(
		log_rows: usize,
		code: LC,
		n_test_queries: usize,
	) -> Result<Self, Error> {
		// Check power of two length because MerkleTreeVCS requires it
		if !code.len().is_power_of_two() {
			return Err(Error::CodeLengthPowerOfTwoRequired);
		}
		let log_len = log2_strict_usize(code.len());
		Self::new(
			log_rows,
			code,
			n_test_queries,
			MerkleTreeVCS::new(log_len, GroestlDigestCompression),
		)
	}
}

impl<F, P, FA, PA, FI, PI, FE, PE, LC, H, VCS> PolyCommitScheme<P, FE>
	for TensorPCS<P, PA, PI, PE, LC, H, VCS>
where
	F: Field,
	P: PackedField<Scalar = F>,
	FA: Field,
	PA: PackedField<Scalar = FA>,
	FI: ExtensionField<F> + ExtensionField<FA>,
	PI: PackedFieldIndexable<Scalar = FI> + PackedExtensionField<P> + PackedExtensionField<PA>,
	FE: ExtensionField<F> + ExtensionField<FI>,
	PE: PackedFieldIndexable<Scalar = FE> + PackedExtensionField<PI>,
	LC: LinearCode<P = PA>,
	H: Hasher<PI>,
	H::Digest: Copy + Default + Send,
	VCS: VectorCommitScheme<H::Digest>,
{
	type Commitment = VCS::Commitment;
	type Committed = (Vec<RowMajorMatrix<PI>>, VCS::Committed);
	type Proof = Proof<'static, PI, PE, VCS::Proof>;
	type Error = Error;

	fn n_vars(&self) -> usize {
		self.log_rows() + self.log_cols()
	}

	#[instrument(skip_all, name = "tensor_pcs::commit")]
	fn commit(
		&self,
		polys: &[MultilinearExtension<P>],
	) -> Result<(Self::Commitment, Self::Committed), Error> {
		for poly in polys {
			if poly.n_vars() != self.n_vars() {
				return Err(Error::IncorrectPolynomialSize {
					expected: self.n_vars(),
				});
			}
		}

		// These conditions are checked by the constructor, so are safe to assert defensively
		debug_assert_eq!(self.code.dim() % PI::WIDTH, 0);

		// Dimensions as an intermediate field matrix.
		let n_rows = 1 << self.log_rows;
		let n_cols_enc = self.code.len();

		let mut encoded_mats = Vec::with_capacity(polys.len());
		let mut all_digests = Vec::with_capacity(polys.len());
		for poly in polys {
			let mut encoded = vec![PI::default(); n_rows * n_cols_enc / PI::WIDTH];
			let poly_vals_packed =
				PI::try_cast_to_ext(poly.evals()).ok_or_else(|| Error::UnalignedMessage)?;

			transpose::transpose(
				PI::unpack_scalars(poly_vals_packed),
				PI::unpack_scalars_mut(&mut encoded[..n_rows * self.code.dim() / PI::WIDTH]),
				1 << self.code.dim_bits(),
				1 << self.log_rows,
			);

			self.code
				.encode_batch_inplace(
					<PI as PackedExtensionField<PA>>::cast_to_bases_mut(&mut encoded),
					self.log_rows + log2_strict_usize(<FI as ExtensionField<FA>>::DEGREE),
				)
				.map_err(|err| Error::EncodeError(Box::new(err)))?;

			let mut digests = vec![H::Digest::default(); n_cols_enc];
			encoded
				.par_chunks_exact(n_rows / PI::WIDTH)
				.map(hash::<_, H>)
				.collect_into_vec(&mut digests);
			all_digests.push(digests);

			let encoded_mat = RowMajorMatrix::new(encoded, n_rows / PI::WIDTH);
			encoded_mats.push(encoded_mat);
		}

		let (commitment, vcs_committed) = self
			.vcs
			.commit_batch(all_digests.into_iter())
			.map_err(|err| Error::VectorCommit(Box::new(err)))?;
		Ok((commitment, (encoded_mats, vcs_committed)))
	}

	/// Generate an evaluation proof at a *random* challenge point.
	///
	/// Follows the notation from Construction 4.6 in [DP23].
	///
	/// Precondition: The queried point must already be observed by the challenger.
	///
	/// [DP23]: https://eprint.iacr.org/2023/630
	#[instrument(skip_all, name = "tensor_pcs::prove_evaluation")]
	fn prove_evaluation<CH>(
		&self,
		challenger: &mut CH,
		committed: &Self::Committed,
		polys: &[MultilinearExtension<P>],
		query: &[FE],
	) -> Result<Self::Proof, Error>
	where
		CH: CanObserve<FE> + CanSample<FE> + CanSampleBits<usize>,
	{
		let n_polys = polys.len();
		let n_challenges = log2_ceil_usize(n_polys);
		let mixing_challenges = challenger.sample_vec(n_challenges);
		let mixing_coefficients =
			&MultilinearQuery::with_full_query(&mixing_challenges)?.into_expansion()[..n_polys];

		let (col_major_mats, ref vcs_committed) = committed;
		if col_major_mats.len() != n_polys {
			return Err(Error::NumBatchedMismatchError {
				err_str: format!("In prove_evaluation: number of polynomials {} must match number of committed matrices {}", n_polys, col_major_mats.len()),
			});
		}

		if query.len() != self.n_vars() {
			return Err(PolynomialError::IncorrectQuerySize {
				expected: self.n_vars(),
			}
			.into());
		}

		let code_len_bits = log2_strict_usize(self.code.len());
		let log_block_size = log2_strict_usize(<FI as ExtensionField<F>>::DEGREE);
		let log_n_cols = self.code.dim_bits() + log_block_size;

		let partial_query = &MultilinearQuery::with_full_query(&query[log_n_cols..])?;
		let ts = polys;
		let t_primes = ts
			.iter()
			.map(|t| t.evaluate_partial_high(partial_query))
			.collect::<Result<Vec<_>, _>>()?;
		let t_prime = mix_t_primes(log_n_cols, &t_primes, mixing_coefficients)?;

		challenger.observe_slice(PE::unpack_scalars(t_prime.evals()));
		let merkle_proofs = repeat_with(|| challenger.sample_bits(code_len_bits))
			.take(self.n_test_queries)
			.map(|index| {
				let vcs_proof = self
					.vcs
					.prove_batch_opening(vcs_committed, index)
					.map_err(|err| Error::VectorCommit(Box::new(err)))?;

				let cols: Vec<_> = col_major_mats
					.iter()
					.map(|col_major_mat| col_major_mat.row_slice(index).to_vec())
					.collect();

				Ok((cols, vcs_proof))
			})
			.collect::<Result<_, Error>>()?;

		Ok(Proof {
			n_polys,
			mixed_t_prime: t_prime,
			vcs_proofs: merkle_proofs,
		})
	}

	/// Verify an evaluation proof at a *random* challenge point.
	///
	/// Follows the notation from Construction 4.6 in [DP23].
	///
	/// Precondition: The queried point must already be observed by the challenger.
	///
	/// [DP23]: https://eprint.iacr.org/2023/630
	#[instrument(skip_all, name = "tensor_pcs::verify_evaluation")]
	fn verify_evaluation<CH>(
		&self,
		challenger: &mut CH,
		commitment: &Self::Commitment,
		query: &[FE],
		proof: Self::Proof,
		values: &[FE],
	) -> Result<(), Error>
	where
		CH: CanObserve<FE> + CanSample<FE> + CanSampleBits<usize>,
	{
		// These are all checked during construction, so it is safe to assert as a defensive
		// measure.
		debug_assert_eq!(self.code.dim() % PI::WIDTH, 0);
		debug_assert_eq!((1 << self.log_rows) % P::WIDTH, 0);
		debug_assert_eq!((1 << self.log_rows) % PI::WIDTH, 0);
		debug_assert_eq!(self.code.dim() % PI::WIDTH, 0);
		debug_assert_eq!(self.code.dim() % PE::WIDTH, 0);

		if values.len() != proof.n_polys {
			return Err(Error::NumBatchedMismatchError {
				err_str:
					format!("In verify_evaluation: proof number of polynomials {} must match number of opened values {}", proof.n_polys, values.len()),
			});
		}

		let n_challenges = log2_ceil_usize(proof.n_polys);
		let mixing_challenges = challenger.sample_vec(n_challenges);
		let mixing_coefficients = &MultilinearQuery::<PE>::with_full_query(&mixing_challenges)?
			.into_expansion()[..proof.n_polys];
		let value =
			inner_product_unchecked(values.iter().copied(), iter_packed_slice(mixing_coefficients));

		if query.len() != self.n_vars() {
			return Err(PolynomialError::IncorrectQuerySize {
				expected: self.n_vars(),
			}
			.into());
		}

		self.check_proof_shape(&proof)?;

		// Code length is checked to be a power of two in the constructor
		let code_len_bits = log2_strict_usize(self.code.len());
		let block_size = <FI as ExtensionField<F>>::DEGREE;
		let log_block_size = log2_strict_usize(block_size);
		let log_n_cols = self.code.dim_bits() + log_block_size;

		let n_rows = 1 << self.log_rows;

		challenger.observe_slice(PE::unpack_scalars(proof.mixed_t_prime.evals()));

		// Check evaluation of t' matches the claimed value
		let multilin_query = MultilinearQuery::<PE>::with_full_query(&query[..log_n_cols])?;
		let computed_value = proof
			.mixed_t_prime
			.evaluate(&multilin_query)
			.expect("query is the correct size by check_proof_shape checks");
		if computed_value != value {
			return Err(VerificationError::IncorrectEvaluation.into());
		}

		// Encode t' into u'
		let mut u_prime = vec![PE::default(); (1 << (code_len_bits + log_block_size)) / PE::WIDTH];
		self.encode_ext(proof.mixed_t_prime.evals(), &mut u_prime)?;

		// Check vector commitment openings.
		let columns = proof
			.vcs_proofs
			.into_iter()
			.map(|(cols, vcs_proof)| {
				let index = challenger.sample_bits(code_len_bits);

				let leaf_digests = cols.iter().map(hash::<_, H>);

				self.vcs
					.verify_batch_opening(commitment, index, vcs_proof, leaf_digests)
					.map_err(|err| Error::VectorCommit(Box::new(err)))?;

				Ok((index, cols))
			})
			.collect::<Result<Vec<_>, Error>>()?;

		// Get the sequence of column tests.
		let column_tests = columns
			.into_iter()
			.flat_map(|(index, cols)| {
				let mut batched_column_test = (0..block_size)
					.map(|j| {
						let u_prime_i = get_packed_slice(&u_prime, index << log_block_size | j);
						let base_cols = Vec::with_capacity(proof.n_polys);
						(u_prime_i, base_cols)
					})
					.collect::<Vec<_>>();

				cols.iter().for_each(|col| {
					// Checked by check_proof_shape
					debug_assert_eq!(col.len(), n_rows / PI::WIDTH);

					// The columns are committed to and provided by the prover as packed vectors of
					// intermediate field elements. We need to transpose them into packed base field
					// elements to perform the consistency checks. Allocate col_transposed as packed
					// intermediate field elements to guarantee alignment.
					let mut col_transposed = vec![PI::default(); n_rows / PI::WIDTH];
					let base_cols =
						PackedExtensionField::<P>::cast_to_bases_mut(&mut col_transposed);
					transpose_scalars(col, base_cols).expect(
						"guaranteed safe because of parameter checks in constructor; \
							alignment is guaranteed the cast from a PI slice",
					);

					debug_assert_eq!(base_cols.len(), n_rows / P::WIDTH * block_size);

					(0..block_size)
						.zip(base_cols.chunks_exact(n_rows / P::WIDTH))
						.for_each(|(j, col)| {
							batched_column_test[j].1.push(col.to_vec());
						});
				});
				batched_column_test
			})
			.collect::<Vec<_>>();

		// Batch evaluate all opened columns
		let multilin_query = MultilinearQuery::<PE>::with_full_query(&query[log_n_cols..])?;
		let incorrect_evaluation = column_tests
			.par_iter()
			.map(|(expected, leaves)| {
				let actual_evals =
					leaves
						.par_iter()
						.map(|leaf| {
							MultilinearExtension::from_values_slice(leaf)
						.expect("leaf is guaranteed power of two length due to check_proof_shape")
						.evaluate(&multilin_query)
						.expect("failed to evaluate")
						})
						.collect::<Vec<_>>();
				(expected, actual_evals)
			})
			.any(|(expected_result, unmixed_actual_results)| {
				// Check that opened column evaluations match u'
				let actual_result = inner_product_unchecked(
					unmixed_actual_results.into_iter(),
					iter_packed_slice(mixing_coefficients),
				);
				actual_result != *expected_result
			});

		if incorrect_evaluation {
			Err(VerificationError::IncorrectPartialEvaluation.into())
		} else {
			Ok(())
		}
	}

	fn proof_size(&self, n_polys: usize) -> usize {
		let t_prime_size = (mem::size_of::<PE>() << self.log_cols()) / PE::WIDTH;
		let column_size = (mem::size_of::<PI>() << self.log_rows()) / PI::WIDTH;
		t_prime_size + (n_polys * column_size + self.vcs.proof_size(n_polys)) * self.n_test_queries
	}
}

impl<F, P, FA, PA, FI, PI, FE, PE, LC, H, VCS> TensorPCS<P, PA, PI, PE, LC, H, VCS>
where
	F: Field,
	P: PackedField<Scalar = F>,
	FA: Field,
	PA: PackedField<Scalar = FA>,
	FI: ExtensionField<F>,
	PI: PackedField<Scalar = FI>,
	FE: ExtensionField<F>,
	PE: PackedField<Scalar = FE>,
	LC: LinearCode<P = PA>,
	H: Hasher<PI>,
	VCS: VectorCommitScheme<H::Digest>,
{
	/// The base-2 logarithm of the number of rows in the committed matrix.
	pub fn log_rows(&self) -> usize {
		self.log_rows
	}

	/// The base-2 logarithm of the number of columns in the pre-encoded matrix.
	pub fn log_cols(&self) -> usize {
		self.code.dim_bits() + log2_strict_usize(FI::DEGREE)
	}
}

impl<F, P, FA, PA, FI, PI, FE, PE, LC, H, VCS> TensorPCS<P, PA, PI, PE, LC, H, VCS>
where
	F: Field,
	P: PackedField<Scalar = F>,
	FA: Field,
	PA: PackedField<Scalar = FA>,
	FI: ExtensionField<F>,
	PI: PackedField<Scalar = FI>,
	FE: ExtensionField<F> + BinaryField,
	PE: PackedField<Scalar = FE>,
	LC: LinearCode<P = PA>,
	H: Hasher<PI>,
	VCS: VectorCommitScheme<H::Digest>,
{
	/// Construct a [`TensorPCS`].
	///
	/// The constructor checks the validity of the type arguments and constructor arguments.
	///
	/// Throws if the linear code block length is not a power of 2.
	/// Throws if the packing width does not divide the code dimension.
	pub fn new(log_rows: usize, code: LC, n_test_queries: usize, vcs: VCS) -> Result<Self, Error> {
		if !code.len().is_power_of_two() {
			// This requirement is just to make sampling indices easier. With a little work it
			// could be relaxed, but power-of-two code lengths are more convenient to work with.
			return Err(Error::CodeLengthPowerOfTwoRequired);
		}

		if !<FI as ExtensionField<F>>::DEGREE.is_power_of_two() {
			return Err(Error::ExtensionDegreePowerOfTwoRequired);
		}
		if !<FE as ExtensionField<F>>::DEGREE.is_power_of_two() {
			return Err(Error::ExtensionDegreePowerOfTwoRequired);
		}

		if (1 << log_rows) % P::WIDTH != 0 {
			return Err(Error::PackingWidthMustDivideNumberOfRows);
		}
		if (1 << log_rows) % PI::WIDTH != 0 {
			return Err(Error::PackingWidthMustDivideNumberOfRows);
		}
		if code.dim() % PI::WIDTH != 0 {
			return Err(Error::PackingWidthMustDivideCodeDimension);
		}
		if code.dim() % PE::WIDTH != 0 {
			return Err(Error::PackingWidthMustDivideCodeDimension);
		}

		Ok(Self {
			log_rows,
			n_test_queries,
			code,
			vcs,
			_p_marker: PhantomData,
			_pi_marker: PhantomData,
			_h_marker: PhantomData,
			_ext_marker: PhantomData,
		})
	}
}

// Helper functions for PolyCommitScheme implementation.
impl<F, P, FA, PA, FI, PI, FE, PE, LC, H, VCS> TensorPCS<P, PA, PI, PE, LC, H, VCS>
where
	F: Field,
	P: PackedField<Scalar = F> + Send,
	FA: Field,
	PA: PackedField<Scalar = FA>,
	FI: ExtensionField<P::Scalar> + ExtensionField<PA::Scalar>,
	PI: PackedFieldIndexable<Scalar = FI>
		+ PackedExtensionField<P>
		+ PackedExtensionField<PA>
		+ Sync,
	FE: ExtensionField<F> + ExtensionField<FI>,
	PE: PackedFieldIndexable<Scalar = FE> + PackedExtensionField<PI>,
	LC: LinearCode<P = PA>,
	H: Hasher<PI>,
	H::Digest: Copy + Default + Send,
	VCS: VectorCommitScheme<H::Digest>,
{
	fn check_proof_shape(&self, proof: &Proof<PI, PE, VCS::Proof>) -> Result<(), Error> {
		let n_rows = 1 << self.log_rows;
		let log_block_size = log2_strict_usize(<FI as ExtensionField<F>>::DEGREE);
		let log_n_cols = self.code.dim_bits() + log_block_size;
		let n_queries = self.n_test_queries;

		if proof.vcs_proofs.len() != n_queries {
			return Err(VerificationError::NumberOfOpeningProofs {
				expected: n_queries,
			}
			.into());
		}
		for (col_idx, (polys_col, _)) in proof.vcs_proofs.iter().enumerate() {
			if polys_col.len() != proof.n_polys {
				return Err(Error::NumBatchedMismatchError {
					err_str: format!(
						"Expected {} polynomials, but VCS proof at col_idx {} found {} polynomials instead",
						proof.n_polys,
						col_idx,
						polys_col.len()
					),
				});
			}

			for (poly_idx, poly_col) in polys_col.iter().enumerate() {
				if poly_col.len() * PI::WIDTH != n_rows {
					return Err(VerificationError::OpenedColumnSize {
						col_index: col_idx,
						poly_index: poly_idx,
						expected: n_rows,
						actual: poly_col.len() * PI::WIDTH,
					}
					.into());
				}
			}
		}

		if proof.mixed_t_prime.n_vars() != log_n_cols {
			return Err(VerificationError::PartialEvaluationSize.into());
		}

		Ok(())
	}

	fn encode_ext(&self, t_prime: &[PE], u_prime: &mut [PE]) -> Result<(), Error> {
		let code_len_bits = log2_strict_usize(self.code.len());
		let block_size = <FI as ExtensionField<F>>::DEGREE;
		let log_block_size = log2_strict_usize(block_size);
		let log_n_cols = self.code.dim_bits() + log_block_size;

		assert_eq!(t_prime.len(), (1 << log_n_cols) / PE::WIDTH);
		assert_eq!(u_prime.len(), (1 << (code_len_bits + log_block_size)) / PE::WIDTH);

		u_prime[..(1 << log_n_cols) / PE::WIDTH].copy_from_slice(t_prime);

		// View u' as a vector of packed base field elements and transpose into packed intermediate
		// field elements in order to apply the extension encoding.
		if log_block_size > 0 {
			// TODO: This requirement is necessary for how we perform the following transpose.
			// It should be relaxed by providing yet another PackedField type as a generic
			// parameter for which this is true.
			assert!(P::WIDTH <= <FE as ExtensionField<F>>::DEGREE);

			let f_view = PackedExtensionField::<P>::cast_to_bases_mut(
				PackedExtensionField::<PI>::cast_to_bases_mut(
					&mut u_prime[..(1 << log_n_cols) / PE::WIDTH],
				),
			);
			f_view
				.par_chunks_exact_mut(block_size)
				.try_for_each(|chunk| square_transpose(log_block_size, chunk))?;
		}

		// View u' as a vector of packed intermediate field elements and batch encode.
		{
			let fi_view = PackedExtensionField::<PI>::cast_to_bases_mut(u_prime);
			let log_batch_size = log2_strict_usize(<FE as ExtensionField<F>>::DEGREE);
			self.code
				.encode_batch_inplace(
					<PI as PackedExtensionField<PA>>::cast_to_bases_mut(fi_view),
					log_batch_size + log2_strict_usize(<FI as ExtensionField<FA>>::DEGREE),
				)
				.map_err(|err| Error::EncodeError(Box::new(err)))?;
		}

		if log_block_size > 0 {
			// TODO: This requirement is necessary for how we perform the following transpose.
			// It should be relaxed by providing yet another PackedField type as a generic
			// parameter for which this is true.
			assert!(P::WIDTH <= <FE as ExtensionField<F>>::DEGREE);

			let f_view = PackedExtensionField::<P>::cast_to_bases_mut(
				PackedExtensionField::<PI>::cast_to_bases_mut(u_prime),
			);
			f_view
				.par_chunks_exact_mut(block_size)
				.try_for_each(|chunk| square_transpose(log_block_size, chunk))?;
		}

		Ok(())
	}
}

/// The basic multilinear polynomial commitment scheme from [DP23].
///
/// The basic scheme follows Construction 3.7. In this case, the encoding alphabet is a subfield of
/// the polynomial's coefficient field.
///
/// [DP23]: <https://eprint.iacr.org/2023/1784>
pub type BasicTensorPCS<P, PA, PE, LC, H, VCS> = TensorPCS<P, PA, P, PE, LC, H, VCS>;

/// The multilinear polynomial commitment scheme from [DP23] with block-level encoding.
///
/// The basic scheme follows Construction 3.11. In this case, the encoding alphabet is an extension
/// field of the polynomial's coefficient field.
///
/// [DP23]: <https://eprint.iacr.org/2023/1784>
pub type BlockTensorPCS<P, PA, PE, LC, H, VCS> = TensorPCS<P, PA, PA, PE, LC, H, VCS>;

pub fn calculate_n_test_queries<F: BinaryField, LC: LinearCode>(
	security_bits: usize,
	log_rows: usize,
	code: &LC,
) -> Result<usize, Error> {
	// Assume we are limited by the non-proximal error term
	let relative_dist = code.min_dist() as f64 / code.len() as f64;
	let non_proximal_per_query_err = 1.0 - (relative_dist / 3.0);
	let mut n_queries =
		(-(security_bits as f64) / non_proximal_per_query_err.log2()).ceil() as usize;
	for _ in 0..10 {
		if calculate_error_bound::<F, _>(log_rows, code, n_queries) >= security_bits {
			return Ok(n_queries);
		}
		n_queries += 1;
	}
	Err(Error::ParameterError)
}

/// Calculates the base-2 log soundness error bound when using general linear codes.
///
/// Returns the number of bits of security achieved with the given parameters. This is computed
/// using the formulae in Section 3.5 of [DP23].
///
/// [DP23]: https://eprint.iacr.org/2023/1784
fn calculate_error_bound<F: BinaryField, LC: LinearCode>(
	log_rows: usize,
	code: &LC,
	n_queries: usize,
) -> usize {
	let e = (code.min_dist() - 1) / 3;
	let relative_dist = code.min_dist() as f64 / code.len() as f64;
	let tensor_batching_err = (2 * log_rows * (e + 1)) as f64 / 2.0_f64.powi(F::N_BITS as i32);
	let non_proximal_err = (1.0 - relative_dist / 3.0).powi(n_queries as i32);
	let proximal_err = (1.0 - 2.0 * relative_dist / 3.0).powi(n_queries as i32);
	let total_err = (tensor_batching_err + non_proximal_err).max(proximal_err);
	-total_err.log2() as usize
}

pub fn calculate_n_test_queries_reed_solomon<F, FE, P>(
	security_bits: usize,
	log_rows: usize,
	code: &ReedSolomonCode<P>,
) -> Result<usize, Error>
where
	F: BinaryField,
	FE: BinaryField + ExtensionField<F>,
	P: PackedField<Scalar = F> + PackedExtensionField<F>,
	P::Scalar: BinaryField,
{
	// Assume we are limited by the non-proximal error term
	let relative_dist = code.min_dist() as f64 / code.len() as f64;
	let non_proximal_per_query_err = 1.0 - (relative_dist / 2.0);
	let mut n_queries =
		(-(security_bits as f64) / non_proximal_per_query_err.log2()).ceil() as usize;
	for _ in 0..10 {
		if calculate_error_bound_reed_solomon::<_, FE, _>(log_rows, code, n_queries)
			>= security_bits
		{
			return Ok(n_queries);
		}
		n_queries += 1;
	}
	Err(Error::ParameterError)
}

/// Calculates the base-2 log soundness error bound when using Reed–Solomon codes.
///
/// Returns the number of bits of security achieved with the given parameters. This is computed
/// using the formulae in Section 3.5 of [DP23]. We use the improved proximity gap result for
/// Reed–Solomon codes, following Remark 3.18 in [DP23].
///
/// [DP23]: https://eprint.iacr.org/2023/1784
fn calculate_error_bound_reed_solomon<F, FE, P>(
	log_rows: usize,
	code: &ReedSolomonCode<P>,
	n_queries: usize,
) -> usize
where
	F: BinaryField,
	FE: BinaryField + ExtensionField<F>,
	P: PackedField<Scalar = F> + PackedExtensionField<F>,
	P::Scalar: BinaryField,
{
	let e = (code.min_dist() - 1) / 2;
	let relative_dist = code.min_dist() as f64 / code.len() as f64;
	let tensor_batching_err = (2 * log_rows * (e + 1)) as f64 / 2.0_f64.powi(FE::N_BITS as i32);
	let non_proximal_err = (1.0 - (relative_dist / 2.0)).powi(n_queries as i32);
	let proximal_err = (1.0 - relative_dist / 2.0).powi(n_queries as i32);
	let total_err = (tensor_batching_err + non_proximal_err).max(proximal_err);
	-total_err.log2() as usize
}

/// Find the TensorPCS parameterization that optimizes proof size.
///
/// This constructs a TensorPCS using a Reed-Solomon code and a Merkle tree using Groestl.
#[allow(clippy::type_complexity)]
pub fn find_proof_size_optimal_pcs<F, P, FA, PA, FI, PI, FE, PE>(
	security_bits: usize,
	n_vars: usize,
	n_polys: usize,
	log_inv_rate: usize,
	conservative_testing: bool,
) -> Option<TensorPCS<P, PA, PI, PE, ReedSolomonCode<PA>, GroestlHasher<PI>, GroestlMerkleTreeVCS>>
where
	F: Field,
	P: PackedField<Scalar = F>,
	FA: BinaryField,
	PA: PackedField<Scalar = FA> + PackedExtensionField<FA>,
	FI: ExtensionField<F> + ExtensionField<FA> + ExtensionField<BinaryField8b>,
	PI: PackedField<Scalar = FI>
		+ PackedExtensionField<BinaryField8b>
		+ PackedExtensionField<FI>
		+ PackedExtensionField<P>
		+ PackedExtensionField<PA>,
	FE: BinaryField + ExtensionField<F> + ExtensionField<FA> + ExtensionField<FI>,
	PE: PackedField<Scalar = FE> + PackedExtensionField<PI> + PackedExtensionField<FE>,
{
	let mut best_proof_size = None;
	let mut best_pcs = None;
	let log_degree = log2_strict_usize(<PI::Scalar as ExtensionField<P::Scalar>>::DEGREE);

	for log_rows in 0..=(n_vars - log_degree) {
		let log_dim = n_vars - log_rows - log_degree;
		let rs_code = match ReedSolomonCode::new(log_dim, log_inv_rate) {
			Ok(rs_code) => rs_code,
			Err(_) => continue,
		};

		let n_test_queries_result = if conservative_testing {
			calculate_n_test_queries::<FE, _>(security_bits, log_rows, &rs_code)
		} else {
			calculate_n_test_queries_reed_solomon::<_, FE, _>(security_bits, log_rows, &rs_code)
		};
		let n_test_queries = match n_test_queries_result {
			Ok(n_test_queries) => n_test_queries,
			Err(_) => continue,
		};

		let pcs = match TensorPCS::<P, PA, PI, PE, _, _, _>::new_using_groestl_merkle_tree(
			log_rows,
			rs_code,
			n_test_queries,
		) {
			Ok(pcs) => pcs,
			Err(_) => continue,
		};

		match best_proof_size {
			None => {
				best_proof_size = Some(pcs.proof_size(n_polys));
				best_pcs = Some(pcs);
			}
			Some(current_best) => {
				let proof_size = pcs.proof_size(n_polys);
				if proof_size < current_best {
					best_proof_size = Some(proof_size);
					best_pcs = Some(pcs);
				}
			}
		}
	}

	best_pcs
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::challenger::HashChallenger;
	use binius_field::{
		BinaryField128b, PackedBinaryField128x1b, PackedBinaryField16x8b, PackedBinaryField1x128b,
		PackedBinaryField4x32b, PackedBinaryField8x16b,
	};
	use rand::{rngs::StdRng, thread_rng, Rng, SeedableRng};

	#[test]
	fn test_simple_commit_prove_verify_without_error() {
		type Packed = PackedBinaryField16x8b;

		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 4, &rs_code)
				.unwrap();
		let pcs =
			<BasicTensorPCS<Packed, Packed, PackedBinaryField1x128b, _, _, _>>::new_using_groestl_merkle_tree(4, rs_code, n_test_queries).unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let evals = repeat_with(|| Packed::random(&mut rng))
			.take((1 << pcs.n_vars()) / Packed::WIDTH)
			.collect::<Vec<_>>();
		let poly = MultilinearExtension::from_values(evals).unwrap();
		let polys = [poly.to_ref()];

		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();

		let multilin_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();
		let value = poly.evaluate(&multilin_query).unwrap();
		let values = vec![value];

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_simple_commit_prove_verify_batch_without_error() {
		type Packed = PackedBinaryField16x8b;

		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 4, &rs_code)
				.unwrap();
		let pcs =
			<BasicTensorPCS<Packed, Packed, PackedBinaryField1x128b, _, _, _>>::new_using_groestl_merkle_tree(4, rs_code, n_test_queries).unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let batch_size = thread_rng().gen_range(1..=10);
		let polys = repeat_with(|| {
			let evals = repeat_with(|| Packed::random(&mut rng))
				.take((1 << pcs.n_vars()) / Packed::WIDTH)
				.collect::<Vec<_>>();
			MultilinearExtension::from_values(evals).unwrap()
		})
		.take(batch_size)
		.collect::<Vec<_>>();

		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();
		let multilin_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();

		let values = polys
			.iter()
			.map(|poly| poly.evaluate(&multilin_query).unwrap())
			.collect::<Vec<_>>();

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_packed_1b_commit_prove_verify_without_error() {
		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 8, &rs_code)
				.unwrap();
		let pcs = <BlockTensorPCS<
			PackedBinaryField128x1b,
			PackedBinaryField16x8b,
			PackedBinaryField1x128b,
			_,
			_,
			_,
		>>::new_using_groestl_merkle_tree(8, rs_code, n_test_queries)
		.unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let evals = repeat_with(|| PackedBinaryField128x1b::random(&mut rng))
			.take((1 << pcs.n_vars()) / PackedBinaryField128x1b::WIDTH)
			.collect::<Vec<_>>();
		let poly = MultilinearExtension::from_values(evals).unwrap();
		let polys = [poly.to_ref()];

		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();

		let multilin_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();
		let value = poly.evaluate(&multilin_query).unwrap();
		let values = vec![value];

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_packed_1b_commit_prove_verify_batch_without_error() {
		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 8, &rs_code)
				.unwrap();
		let pcs = <BlockTensorPCS<
			PackedBinaryField128x1b,
			PackedBinaryField16x8b,
			PackedBinaryField1x128b,
			_,
			_,
			_,
		>>::new_using_groestl_merkle_tree(8, rs_code, n_test_queries)
		.unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let batch_size = thread_rng().gen_range(1..=10);
		let polys = repeat_with(|| {
			let evals = repeat_with(|| PackedBinaryField128x1b::random(&mut rng))
				.take((1 << pcs.n_vars()) / PackedBinaryField128x1b::WIDTH)
				.collect::<Vec<_>>();
			MultilinearExtension::from_values(evals).unwrap()
		})
		.take(batch_size)
		.collect::<Vec<_>>();
		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();
		let multilinear_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();

		let values = polys
			.iter()
			.map(|poly| poly.evaluate(&multilinear_query).unwrap())
			.collect::<Vec<_>>();

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_packed_32b_commit_prove_verify_without_error() {
		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 8, &rs_code)
				.unwrap();
		let pcs = <BasicTensorPCS<
			PackedBinaryField4x32b,
			PackedBinaryField16x8b,
			PackedBinaryField1x128b,
			_,
			_,
			_,
		>>::new_using_groestl_merkle_tree(8, rs_code, n_test_queries)
		.unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let evals = repeat_with(|| PackedBinaryField4x32b::random(&mut rng))
			.take((1 << pcs.n_vars()) / PackedBinaryField4x32b::WIDTH)
			.collect::<Vec<_>>();
		let poly = MultilinearExtension::from_values(evals).unwrap();
		let polys = [poly.to_ref()];

		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();

		let multilin_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();
		let value = poly.evaluate(&multilin_query).unwrap();
		let values = vec![value];

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_packed_32b_commit_prove_verify_batch_without_error() {
		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 8, &rs_code)
				.unwrap();
		let pcs = <BasicTensorPCS<
			PackedBinaryField4x32b,
			PackedBinaryField16x8b,
			PackedBinaryField1x128b,
			_,
			_,
			_,
		>>::new_using_groestl_merkle_tree(8, rs_code, n_test_queries)
		.unwrap();

		let mut rng = StdRng::seed_from_u64(0);
		let batch_size = thread_rng().gen_range(1..=10);
		let polys = repeat_with(|| {
			let evals = repeat_with(|| PackedBinaryField4x32b::random(&mut rng))
				.take((1 << pcs.n_vars()) / PackedBinaryField4x32b::WIDTH)
				.collect::<Vec<_>>();
			MultilinearExtension::from_values(evals).unwrap()
		})
		.take(batch_size)
		.collect::<Vec<_>>();
		let (commitment, committed) = pcs.commit(&polys).unwrap();

		let mut challenger = <HashChallenger<_, GroestlHasher<_>>>::new();
		let query = repeat_with(|| challenger.sample())
			.take(pcs.n_vars())
			.collect::<Vec<_>>();
		let multilin_query =
			MultilinearQuery::<PackedBinaryField1x128b>::with_full_query(&query).unwrap();

		let values = polys
			.iter()
			.map(|poly| poly.evaluate(&multilin_query).unwrap())
			.collect::<Vec<_>>();

		let mut prove_challenger = challenger.clone();
		let proof = pcs
			.prove_evaluation(&mut prove_challenger, &committed, &polys, &query)
			.unwrap();

		let mut verify_challenger = challenger.clone();
		pcs.verify_evaluation(&mut verify_challenger, &commitment, &query, proof, &values)
			.unwrap();
	}

	#[test]
	fn test_proof_size() {
		let rs_code = ReedSolomonCode::new(5, 2).unwrap();
		let n_test_queries =
			calculate_n_test_queries_reed_solomon::<_, BinaryField128b, _>(100, 8, &rs_code)
				.unwrap();
		let pcs = <BasicTensorPCS<
			PackedBinaryField4x32b,
			PackedBinaryField16x8b,
			PackedBinaryField1x128b,
			_,
			_,
			_,
		>>::new_using_groestl_merkle_tree(8, rs_code, n_test_queries)
		.unwrap();

		assert_eq!(pcs.proof_size(1), 182720);
		assert_eq!(pcs.proof_size(2), 332224);
	}

	#[test]
	fn test_proof_size_optimal_block_pcs() {
		let pcs = find_proof_size_optimal_pcs::<
			_,
			PackedBinaryField128x1b,
			_,
			PackedBinaryField8x16b,
			_,
			PackedBinaryField8x16b,
			_,
			PackedBinaryField1x128b,
		>(100, 28, 1, 2, false)
		.unwrap();
		assert_eq!(pcs.n_vars(), 28);
		assert_eq!(pcs.log_rows(), 12);
		assert_eq!(pcs.log_cols(), 16);

		// Matrix should be wider with more polynomials per batch.
		let pcs = find_proof_size_optimal_pcs::<
			_,
			PackedBinaryField128x1b,
			_,
			PackedBinaryField8x16b,
			_,
			PackedBinaryField8x16b,
			_,
			PackedBinaryField1x128b,
		>(100, 28, 8, 2, false)
		.unwrap();
		assert_eq!(pcs.n_vars(), 28);
		assert_eq!(pcs.log_rows(), 10);
		assert_eq!(pcs.log_cols(), 18);
	}

	#[test]
	fn test_proof_size_optimal_basic_pcs() {
		let pcs = find_proof_size_optimal_pcs::<
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField1x128b,
		>(100, 28, 1, 2, false)
		.unwrap();
		assert_eq!(pcs.n_vars(), 28);
		assert_eq!(pcs.log_rows(), 11);
		assert_eq!(pcs.log_cols(), 17);

		// Matrix should be wider with more polynomials per batch.
		let pcs = find_proof_size_optimal_pcs::<
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField4x32b,
			_,
			PackedBinaryField1x128b,
		>(100, 28, 8, 2, false)
		.unwrap();
		assert_eq!(pcs.n_vars(), 28);
		assert_eq!(pcs.log_rows(), 10);
		assert_eq!(pcs.log_cols(), 18);
	}
}

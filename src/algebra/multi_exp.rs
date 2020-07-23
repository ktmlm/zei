/*
 * Based on dalek-cryptography/curve25519-dalek implementation of Pippenger algorithm for multi-exponentiations
 */
use crate::algebra::groups::{scalar_to_radix_2_power_w, Group, Scalar};
use std::borrow::Borrow;

pub trait MultiExp<S>: Group<S> {
  fn naive_multi_exp<I, H>(scalars: I, points: H) -> Self
    where I: IntoIterator,
          I::Item: Borrow<S>,
          H: IntoIterator,
          H::Item: Borrow<Self>;
  fn multi_exp<I, H>(scalars: I, points: H) -> Self
    where I: IntoIterator,
          I::Item: Borrow<S>,
          H: IntoIterator,
          H::Item: Borrow<Self>;
  fn vartime_multi_exp(scalars: &[&S], points: &[&Self]) -> Self;
}

impl<S: Scalar, G: Group<S>> MultiExp<S> for G {
  fn naive_multi_exp<I, H>(scalars: I, points: H) -> Self
    where I: IntoIterator,
          I::Item: Borrow<S>,
          H: IntoIterator,
          H::Item: Borrow<Self>
  {
    let mut r = Self::get_identity();
    for (s, p) in scalars.into_iter().zip(points.into_iter()) {
      r = r.add(&p.borrow().mul(s.borrow()))
    }
    r
  }

  fn multi_exp<I, H>(scalars: I, points: H) -> Self
    where I: IntoIterator,
          I::Item: Borrow<S>,
          H: IntoIterator,
          H::Item: Borrow<Self>
  {
    Self::naive_multi_exp(scalars, points)
  }
  fn vartime_multi_exp(scalars: &[&S], points: &[&G]) -> Self {
    pippenger::<S, G>(scalars, points)
  }
}

fn pippenger<S: Scalar, G: Group<S>>(scalars: &[&S], elems: &[&G]) -> G {
  let size = scalars.len();

  let w = if size < 500 {
    6
  } else if size < 800 {
    7
  } else {
    8
  };

  let two_power_w: usize = 1 << w;
  let digits_vec: Vec<Vec<i8>> = scalars.iter()
                                        .map(|s| scalar_to_radix_2_power_w::<S>(s, w))
                                        .collect();
  // TODO (fernando) remove this clone
  let mut digits_count = 0;
  for digits in digits_vec.iter() {
    if digits.len() > digits_count {
      digits_count = digits.len();
    }
  }

  // init all the buckets
  let mut buckets: Vec<_> = (0..two_power_w / 2).map(|_| G::get_identity()).collect();

  let mut cols = (0..digits_count).rev().map(|index| {
                                          // empty each bucket
                                          for b in buckets.iter_mut() {
                                            *b = G::get_identity();
                                          }
                                          for (digits, elem) in digits_vec.iter().zip(elems) {
                                            if index >= digits.len() {
                                              continue;
                                            }
                                            let digit = digits[index];
                                            if digit > 0 {
                                              let b_index = (digit - 1) as usize;
                                              buckets[b_index] = buckets[b_index].add(elem);
                                            }
                                            if digit < 0 {
                                              let b_index = (-digit - 1) as usize;
                                              buckets[b_index] = buckets[b_index].sub(elem);
                                            }
                                          }
                                          let mut intermediate_sum =
                                            buckets[buckets.len() - 1].clone();
                                          let mut sum = buckets[buckets.len() - 1].clone();
                                          for i in (0..buckets.len() - 1).rev() {
                                            intermediate_sum = intermediate_sum.add(&buckets[i]);
                                            sum = sum.add(&intermediate_sum);
                                          }
                                          sum
                                        });

  let two_power_w_int = Scalar::from_u64(two_power_w as u64);
  let hi_col = cols.next().unwrap(); // TODO check if unwrap is safe in all cases
  cols.fold(hi_col, |total, p| total.mul(&two_power_w_int).add(&p))
}

#[cfg(test)]
mod tests {
  use crate::algebra::bls12_381::{BLSGt, BLSScalar, BLSG1, BLSG2};
  use crate::algebra::groups::{Group, Scalar};
  use crate::algebra::multi_exp::MultiExp;

  #[test]
  fn test_multiexp_ristretto() {
    run_multiexp_test::<curve25519_dalek::scalar::Scalar,
                      curve25519_dalek::ristretto::RistrettoPoint>();
  }
  #[test]
  fn test_multiexp_blsg1() {
    run_multiexp_test::<BLSScalar, BLSG1>();
  }
  #[test]
  fn test_multiexp_blsg2() {
    run_multiexp_test::<BLSScalar, BLSG2>();
  }
  #[test]
  fn test_multiexp_blsgt() {
    run_multiexp_test::<BLSScalar, BLSGt>();
  }

  fn run_multiexp_test<S: Scalar, G: Group<S>>() {
    let g1 = G::get_base();
    let zero = S::from_u32(0);
    let g = G::vartime_multi_exp(&[&zero], &[&g1]);
    assert_eq!(g, G::get_identity());

    let g1 = G::get_base();
    let one = Scalar::from_u32(1);
    let g = G::vartime_multi_exp(&[&one], &[&g1]);
    assert_eq!(g, G::get_base());

    let g1 = G::get_base();
    let g1p = G::get_base();
    let one = Scalar::from_u32(1);
    let zero = Scalar::from_u32(0);
    let g = G::vartime_multi_exp(&[&one, &zero], &[&g1, &g1p]);
    assert_eq!(g, G::get_base());

    let g1 = G::get_base();
    let g2 = g1.add(&g1);
    let g3 = g1.mul(&Scalar::from_u32(500));
    let thousand = Scalar::from_u32(1000);
    let two = Scalar::from_u32(2);
    let three = Scalar::from_u32(3);
    let g = G::vartime_multi_exp(&[&thousand, &two, &three], &[&g1, &g2, &g3]);
    let expected = G::get_base().mul(&Scalar::from_u32(1000 + 4 + 1500));
    assert_eq!(g, expected);
  }
}
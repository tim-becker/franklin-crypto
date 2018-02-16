use pairing::{
    Engine,
    Field
};

use bellman::{
    SynthesisError,
    ConstraintSystem
};

use super::{
    Assignment
};

use super::num::AllocatedNum;

use ::jubjub::{
    JubjubEngine,
    JubjubParams,
    FixedGenerators
};

use super::lookup::{
    lookup3_xy
};

use super::boolean::Boolean;

pub struct EdwardsPoint<E: Engine> {
    pub x: AllocatedNum<E>,
    pub y: AllocatedNum<E>
}

impl<E: Engine> Clone for EdwardsPoint<E> {
    fn clone(&self) -> Self {
        EdwardsPoint {
            x: self.x.clone(),
            y: self.y.clone()
        }
    }
}

/// Perform a fixed-base scalar multiplication with
/// `by` being in little-endian bit order. `by` must
/// be a multiple of 3.
pub fn fixed_base_multiplication<E, CS>(
    mut cs: CS,
    base: FixedGenerators,
    by: &[Boolean],
    params: &E::Params
) -> Result<EdwardsPoint<E>, SynthesisError>
    where CS: ConstraintSystem<E>,
          E: JubjubEngine
{
    // We're going to chunk the scalar into 3-bit windows,
    // so let's force the caller to supply the right number
    // of bits for our lookups.
    assert!(by.len() % 3 == 0);

    // Represents the result of the multiplication
    let mut result = None;

    for (i, (chunk, window)) in by.chunks(3)
                                  .zip(params.circuit_generators(base).iter())
                                  .enumerate()
    {
        let (x, y) = lookup3_xy(
            cs.namespace(|| format!("window table lookup {}", i)),
            chunk,
            window
        )?;

        let p = EdwardsPoint {
            x: x,
            y: y
        };

        if result.is_none() {
            result = Some(p);
        } else {
            result = Some(result.unwrap().add(
                cs.namespace(|| format!("addition {}", i)),
                &p,
                params
            )?);
        }
    }

    Ok(result.get()?.clone())
}

impl<E: JubjubEngine> EdwardsPoint<E> {
    /// This extracts the x-coordinate, which is an injective
    /// encoding for elements of the prime order subgroup.
    pub fn into_num(&self) -> AllocatedNum<E> {
        self.x.clone()
    }

    /// Returns `self` if condition is true, and the neutral
    /// element (0, 1) otherwise.
    pub fn conditionally_select<CS>(
        &self,
        mut cs: CS,
        condition: &Boolean
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Compute x' = self.x if condition, and 0 otherwise
        let x_prime = AllocatedNum::alloc(cs.namespace(|| "x'"), || {
            if *condition.get_value().get()? {
                Ok(*self.x.get_value().get()?)
            } else {
                Ok(E::Fr::zero())
            }
        })?;

        // condition * x = x'
        // if condition is 0, x' must be 0
        // if condition is 1, x' must be x
        let one = CS::one();
        cs.enforce(
            || "x' computation",
            |lc| lc + self.x.get_variable(),
            |_| condition.lc(one, E::Fr::one()),
            |lc| lc + x_prime.get_variable()
        );

        // Compute y' = self.y if condition, and 1 otherwise
        let y_prime = AllocatedNum::alloc(cs.namespace(|| "y'"), || {
            if *condition.get_value().get()? {
                Ok(*self.y.get_value().get()?)
            } else {
                Ok(E::Fr::one())
            }
        })?;

        // condition * y = y' - (1 - condition)
        // if condition is 0, y' must be 1
        // if condition is 1, y' must be y
        cs.enforce(
            || "y' computation",
            |lc| lc + self.y.get_variable(),
            |_| condition.lc(one, E::Fr::one()),
            |lc| lc + y_prime.get_variable()
                                                - &condition.not().lc(one, E::Fr::one())
        );

        Ok(EdwardsPoint {
            x: x_prime,
            y: y_prime
        })
    }

    /// Performs a scalar multiplication of this twisted Edwards
    /// point by a scalar represented as a sequence of booleans
    /// in little-endian bit order.
    pub fn mul<CS>(
        &self,
        mut cs: CS,
        by: &[Boolean],
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Represents the current "magnitude" of the base
        // that we're operating over. Starts at self,
        // then 2*self, then 4*self, ...
        let mut curbase = None;

        // Represents the result of the multiplication
        let mut result = None;

        for (i, bit) in by.iter().enumerate() {
            if curbase.is_none() {
                curbase = Some(self.clone());
            } else {
                // Double the previous value
                curbase = Some(
                    curbase.unwrap()
                           .double(cs.namespace(|| format!("doubling {}", i)), params)?
                );
            }

            // Represents the select base. If the bit for this magnitude
            // is true, this will return `curbase`. Otherwise it will
            // return the neutral element, which will have no effect on
            // the result.
            let thisbase = curbase.as_ref()
                                  .unwrap()
                                  .conditionally_select(
                                      cs.namespace(|| format!("selection {}", i)),
                                      bit
                                  )?;

            if result.is_none() {
                result = Some(thisbase);
            } else {
                result = Some(result.unwrap().add(
                    cs.namespace(|| format!("addition {}", i)),
                    &thisbase,
                    params
                )?);
            }
        }

        Ok(result.get()?.clone())
    }

    pub fn interpret<CS>(
        mut cs: CS,
        x: &AllocatedNum<E>,
        y: &AllocatedNum<E>,
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // -x^2 + y^2 = 1 + dx^2y^2

        let x2 = x.square(cs.namespace(|| "x^2"))?;
        let y2 = y.square(cs.namespace(|| "y^2"))?;
        let x2y2 = x2.mul(cs.namespace(|| "x^2 y^2"), &y2)?;

        let one = CS::one();
        cs.enforce(
            || "on curve check",
            |lc| lc - x2.get_variable()
                    + y2.get_variable(),
            |lc| lc + one,
            |lc| lc + one
                    + (*params.edwards_d(), x2y2.get_variable())
        );

        Ok(EdwardsPoint {
            x: x.clone(),
            y: y.clone()
        })
    }

    pub fn double<CS>(
        &self,
        cs: CS,
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        self.add(cs, self, params)
    }

    /// Perform addition between any two points
    pub fn add<CS>(
        &self,
        mut cs: CS,
        other: &Self,
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Compute U = (x1 + y1) * (x2 + y2)
        let u = AllocatedNum::alloc(cs.namespace(|| "U"), || {
            let mut t0 = *self.x.get_value().get()?;
            t0.add_assign(self.y.get_value().get()?);

            let mut t1 = *other.x.get_value().get()?;
            t1.add_assign(other.y.get_value().get()?);

            t0.mul_assign(&t1);

            Ok(t0)
        })?;

        cs.enforce(
            || "U computation",
            |lc| lc + self.x.get_variable()
                    + self.y.get_variable(),
            |lc| lc + other.x.get_variable()
                    + other.y.get_variable(),
            |lc| lc + u.get_variable()
        );

        // Compute A = y2 * x1
        let a = other.y.mul(cs.namespace(|| "A computation"), &self.x)?;

        // Compute B = x2 * y1
        let b = other.x.mul(cs.namespace(|| "B computation"), &self.y)?;

        // Compute C = d*A*B
        let c = AllocatedNum::alloc(cs.namespace(|| "C"), || {
            let mut t0 = *a.get_value().get()?;
            t0.mul_assign(b.get_value().get()?);
            t0.mul_assign(params.edwards_d());

            Ok(t0)
        })?;

        cs.enforce(
            || "C computation",
            |lc| lc + (*params.edwards_d(), a.get_variable()),
            |lc| lc + b.get_variable(),
            |lc| lc + c.get_variable()
        );

        // Compute x3 = (A + B) / (1 + C)
        let x3 = AllocatedNum::alloc(cs.namespace(|| "x3"), || {
            let mut t0 = *a.get_value().get()?;
            t0.add_assign(b.get_value().get()?);

            let mut t1 = E::Fr::one();
            t1.add_assign(c.get_value().get()?);

            match t1.inverse() {
                Some(t1) => {
                    t0.mul_assign(&t1);

                    Ok(t0)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        let one = CS::one();
        cs.enforce(
            || "x3 computation",
            |lc| lc + one + c.get_variable(),
            |lc| lc + x3.get_variable(),
            |lc| lc + a.get_variable()
                    + b.get_variable()
        );

        // Compute y3 = (U - A - B) / (1 - C)
        let y3 = AllocatedNum::alloc(cs.namespace(|| "y3"), || {
            let mut t0 = *u.get_value().get()?;
            t0.sub_assign(a.get_value().get()?);
            t0.sub_assign(b.get_value().get()?);

            let mut t1 = E::Fr::one();
            t1.sub_assign(c.get_value().get()?);

            match t1.inverse() {
                Some(t1) => {
                    t0.mul_assign(&t1);

                    Ok(t0)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        cs.enforce(
            || "y3 computation",
            |lc| lc + one - c.get_variable(),
            |lc| lc + y3.get_variable(),
            |lc| lc + u.get_variable()
                    - a.get_variable()
                    - b.get_variable()
        );

        Ok(EdwardsPoint {
            x: x3,
            y: y3
        })
    }
}

pub struct MontgomeryPoint<E: Engine> {
    x: AllocatedNum<E>,
    y: AllocatedNum<E>
}

impl<E: JubjubEngine> MontgomeryPoint<E> {
    /// Converts an element in the prime order subgroup into
    /// a point in the birationally equivalent twisted
    /// Edwards curve.
    pub fn into_edwards<CS>(
        &self,
        mut cs: CS,
        params: &E::Params
    ) -> Result<EdwardsPoint<E>, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Compute u = (scale*x) / y
        let u = AllocatedNum::alloc(cs.namespace(|| "u"), || {
            let mut t0 = *self.x.get_value().get()?;
            t0.mul_assign(params.scale());

            match self.y.get_value().get()?.inverse() {
                Some(invy) => {
                    t0.mul_assign(&invy);

                    Ok(t0)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        cs.enforce(
            || "u computation",
            |lc| lc + self.y.get_variable(),
            |lc| lc + u.get_variable(),
            |lc| lc + (*params.scale(), self.x.get_variable())
        );

        // Compute v = (x - 1) / (x + 1)
        let v = AllocatedNum::alloc(cs.namespace(|| "v"), || {
            let mut t0 = *self.x.get_value().get()?;
            let mut t1 = t0;
            t0.sub_assign(&E::Fr::one());
            t1.add_assign(&E::Fr::one());

            match t1.inverse() {
                Some(t1) => {
                    t0.mul_assign(&t1);

                    Ok(t0)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        let one = CS::one();
        cs.enforce(
            || "v computation",
            |lc| lc + self.x.get_variable()
                    + one,
            |lc| lc + v.get_variable(),
            |lc| lc + self.x.get_variable()
                    - one,
        );

        Ok(EdwardsPoint {
            x: u,
            y: v
        })
    }

    /// Interprets an (x, y) pair as a point
    /// in Montgomery, does not check that it's
    /// on the curve. Useful for constants and
    /// window table lookups.
    pub fn interpret_unchecked(
        x: AllocatedNum<E>,
        y: AllocatedNum<E>
    ) -> Self
    {
        MontgomeryPoint {
            x: x,
            y: y
        }
    }

    /// Performs an affine point addition, not defined for
    /// coincident points.
    pub fn add<CS>(
        &self,
        mut cs: CS,
        other: &Self,
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Compute lambda = (y' - y) / (x' - x)
        let lambda = AllocatedNum::alloc(cs.namespace(|| "lambda"), || {
            let mut n = *other.y.get_value().get()?;
            n.sub_assign(self.y.get_value().get()?);

            let mut d = *other.x.get_value().get()?;
            d.sub_assign(self.x.get_value().get()?);

            match d.inverse() {
                Some(d) => {
                    n.mul_assign(&d);
                    Ok(n)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        cs.enforce(
            || "evaluate lambda",
            |lc| lc + other.x.get_variable()
                    - self.x.get_variable(),

            |lc| lc + lambda.get_variable(),

            |lc| lc + other.y.get_variable()
                    - self.y.get_variable()
        );

        // Compute x'' = lambda^2 - A - x - x'
        let xprime = AllocatedNum::alloc(cs.namespace(|| "xprime"), || {
            let mut t0 = *lambda.get_value().get()?;
            t0.square();
            t0.sub_assign(params.montgomery_a());
            t0.sub_assign(self.x.get_value().get()?);
            t0.sub_assign(other.x.get_value().get()?);

            Ok(t0)
        })?;

        // (lambda) * (lambda) = (A + x + x' + x'')
        let one = CS::one();
        cs.enforce(
            || "evaluate xprime",
            |lc| lc + lambda.get_variable(),
            |lc| lc + lambda.get_variable(),
            |lc| lc + (*params.montgomery_a(), one)
                    + self.x.get_variable()
                    + other.x.get_variable()
                    + xprime.get_variable()
        );

        // Compute y' = -(y + lambda(x' - x))
        let yprime = AllocatedNum::alloc(cs.namespace(|| "yprime"), || {
            let mut t0 = *xprime.get_value().get()?;
            t0.sub_assign(self.x.get_value().get()?);
            t0.mul_assign(lambda.get_value().get()?);
            t0.add_assign(self.y.get_value().get()?);
            t0.negate();

            Ok(t0)
        })?;

        // y' + y = lambda(x - x')
        cs.enforce(
            || "evaluate yprime",
            |lc| lc + self.x.get_variable()
                    - xprime.get_variable(),

            |lc| lc + lambda.get_variable(),

            |lc| lc + yprime.get_variable()
                    + self.y.get_variable()
        );

        Ok(MontgomeryPoint {
            x: xprime,
            y: yprime
        })
    }

    /// Performs an affine point doubling, not defined for
    /// the point of order two (0, 0).
    pub fn double<CS>(
        &self,
        mut cs: CS,
        params: &E::Params
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // Square x
        let xx = self.x.square(&mut cs)?;

        // Compute lambda = (3.xx + 2.A.x + 1) / 2.y
        let lambda = AllocatedNum::alloc(cs.namespace(|| "lambda"), || {
            let mut t0 = *xx.get_value().get()?;
            let mut t1 = t0;
            t0.double(); // t0 = 2.xx
            t0.add_assign(&t1); // t0 = 3.xx
            t1 = *self.x.get_value().get()?; // t1 = x
            t1.mul_assign(params.montgomery_2a()); // t1 = 2.A.x
            t0.add_assign(&t1);
            t0.add_assign(&E::Fr::one());
            t1 = *self.y.get_value().get()?; // t1 = y
            t1.double(); // t1 = 2.y
            match t1.inverse() {
                Some(t1) => {
                    t0.mul_assign(&t1);

                    Ok(t0)
                },
                None => {
                    Err(SynthesisError::DivisionByZero)
                }
            }
        })?;

        // (2.y) * (lambda) = (3.xx + 2.A.x + 1)
        let one = CS::one();
        cs.enforce(
            || "evaluate lambda",
            |lc| lc + self.y.get_variable()
                    + self.y.get_variable(),

            |lc| lc + lambda.get_variable(),

            |lc| lc + xx.get_variable()
                    + xx.get_variable()
                    + xx.get_variable()
                    + (*params.montgomery_2a(), self.x.get_variable())
                    + one
        );

        // Compute x' = (lambda^2) - A - 2.x
        let xprime = AllocatedNum::alloc(cs.namespace(|| "xprime"), || {
            let mut t0 = *lambda.get_value().get()?;
            t0.square();
            t0.sub_assign(params.montgomery_a());
            t0.sub_assign(self.x.get_value().get()?);
            t0.sub_assign(self.x.get_value().get()?);

            Ok(t0)
        })?;

        // (lambda) * (lambda) = (A + 2.x + x')
        cs.enforce(
            || "evaluate xprime",
            |lc| lc + lambda.get_variable(),
            |lc| lc + lambda.get_variable(),
            |lc| lc + (*params.montgomery_a(), one)
                    + self.x.get_variable()
                    + self.x.get_variable()
                    + xprime.get_variable()
        );

        // Compute y' = -(y + lambda(x' - x))
        let yprime = AllocatedNum::alloc(cs.namespace(|| "yprime"), || {
            let mut t0 = *xprime.get_value().get()?;
            t0.sub_assign(self.x.get_value().get()?);
            t0.mul_assign(lambda.get_value().get()?);
            t0.add_assign(self.y.get_value().get()?);
            t0.negate();

            Ok(t0)
        })?;

        // y' + y = lambda(x - x')
        cs.enforce(
            || "evaluate yprime",
            |lc| lc + self.x.get_variable()
                    - xprime.get_variable(),

            |lc| lc + lambda.get_variable(),

            |lc| lc + yprime.get_variable()
                    + self.y.get_variable()
        );

        Ok(MontgomeryPoint {
            x: xprime,
            y: yprime
        })
    }
}

#[cfg(test)]
mod test {
    use bellman::{ConstraintSystem};
    use rand::{XorShiftRng, SeedableRng, Rand, Rng};
    use pairing::bls12_381::{Bls12, Fr};
    use pairing::{BitIterator, Field, PrimeField};
    use ::circuit::test::*;
    use ::jubjub::{
        montgomery,
        edwards,
        JubjubBls12,
        JubjubParams,
        FixedGenerators
    };
    use ::jubjub::fs::Fs;
    use super::{
        MontgomeryPoint,
        EdwardsPoint,
        AllocatedNum,
        fixed_base_multiplication
    };
    use super::super::boolean::{
        Boolean,
        AllocatedBit
    };

    #[test]
    fn test_into_edwards() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let p = montgomery::Point::<Bls12, _>::rand(rng, params);
            let (u, v) = edwards::Point::from_montgomery(&p, params).into_xy();
            let (x, y) = p.into_xy().unwrap();

            let numx = AllocatedNum::alloc(cs.namespace(|| "mont x"), || {
                Ok(x)
            }).unwrap();
            let numy = AllocatedNum::alloc(cs.namespace(|| "mont y"), || {
                Ok(y)
            }).unwrap();

            let p = MontgomeryPoint::interpret_unchecked(numx, numy);

            let q = p.into_edwards(&mut cs, params).unwrap();

            assert!(cs.is_satisfied());
            assert!(q.x.get_value().unwrap() == u);
            assert!(q.y.get_value().unwrap() == v);

            cs.set("u/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied().unwrap(), "u computation");
            cs.set("u/num", u);
            assert!(cs.is_satisfied());

            cs.set("v/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied().unwrap(), "v computation");
            cs.set("v/num", v);
            assert!(cs.is_satisfied());
        }
    }

    #[test]
    fn test_interpret() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let p = edwards::Point::<Bls12, _>::rand(rng, &params);
            let (x, y) = p.into_xy();

            let mut cs = TestConstraintSystem::<Bls12>::new();
            let numx = AllocatedNum::alloc(cs.namespace(|| "x"), || {
                Ok(x)
            }).unwrap();
            let numy = AllocatedNum::alloc(cs.namespace(|| "y"), || {
                Ok(y)
            }).unwrap();

            let p = EdwardsPoint::interpret(&mut cs, &numx, &numy, &params).unwrap();

            assert!(cs.is_satisfied());
            assert_eq!(p.x.get_value().unwrap(), x);
            assert_eq!(p.y.get_value().unwrap(), y);
        }

        // Random (x, y) are unlikely to be on the curve.
        for _ in 0..100 {
            let x = rng.gen();
            let y = rng.gen();

            let mut cs = TestConstraintSystem::<Bls12>::new();
            let numx = AllocatedNum::alloc(cs.namespace(|| "x"), || {
                Ok(x)
            }).unwrap();
            let numy = AllocatedNum::alloc(cs.namespace(|| "y"), || {
                Ok(y)
            }).unwrap();

            EdwardsPoint::interpret(&mut cs, &numx, &numy, &params).unwrap();

            assert_eq!(cs.which_is_unsatisfied().unwrap(), "on curve check");
        }
    }

    #[test]
    fn test_doubling_order_2() {
        let params = &JubjubBls12::new();

        let mut cs = TestConstraintSystem::<Bls12>::new();

        let x = AllocatedNum::alloc(cs.namespace(|| "x"), || {
            Ok(Fr::zero())
        }).unwrap();
        let y = AllocatedNum::alloc(cs.namespace(|| "y"), || {
            Ok(Fr::zero())
        }).unwrap();

        let p = MontgomeryPoint {
            x: x,
            y: y
        };

        assert!(p.double(&mut cs, params).is_err());
    }

    #[test]
    fn test_edwards_fixed_base_multiplication()  {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let p = params.generator(FixedGenerators::NoteCommitmentRandomization);
            let s = Fs::rand(rng);
            let q = p.mul(s, params);
            let (x1, y1) = q.into_xy();

            let mut s_bits = BitIterator::new(s.into_repr()).collect::<Vec<_>>();
            s_bits.reverse();
            s_bits.truncate(Fs::NUM_BITS as usize);

            let s_bits = s_bits.into_iter()
                               .enumerate()
                               .map(|(i, b)| AllocatedBit::alloc(cs.namespace(|| format!("scalar bit {}", i)), Some(b)).unwrap())
                               .map(|v| Boolean::from(v))
                               .collect::<Vec<_>>();

            let q = fixed_base_multiplication(
                cs.namespace(|| "multiplication"),
                FixedGenerators::NoteCommitmentRandomization,
                &s_bits,
                params
            ).unwrap();

            assert_eq!(q.x.get_value().unwrap(), x1);
            assert_eq!(q.y.get_value().unwrap(), y1);
        }
    }

    #[test]
    fn test_edwards_multiplication() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let p = edwards::Point::<Bls12, _>::rand(rng, params);
            let s = Fs::rand(rng);
            let q = p.mul(s, params);

            let (x0, y0) = p.into_xy();
            let (x1, y1) = q.into_xy();

            let num_x0 = AllocatedNum::alloc(cs.namespace(|| "x0"), || {
                Ok(x0)
            }).unwrap();
            let num_y0 = AllocatedNum::alloc(cs.namespace(|| "y0"), || {
                Ok(y0)
            }).unwrap();

            let p = EdwardsPoint {
                x: num_x0,
                y: num_y0
            };

            let mut s_bits = BitIterator::new(s.into_repr()).collect::<Vec<_>>();
            s_bits.reverse();
            s_bits.truncate(Fs::NUM_BITS as usize);

            let s_bits = s_bits.into_iter()
                               .enumerate()
                               .map(|(i, b)| AllocatedBit::alloc(cs.namespace(|| format!("scalar bit {}", i)), Some(b)).unwrap())
                               .map(|v| Boolean::from(v))
                               .collect::<Vec<_>>();

            let q = p.mul(
                cs.namespace(|| "scalar mul"),
                &s_bits,
                params
            ).unwrap();

            assert!(cs.is_satisfied());

            assert_eq!(
                q.x.get_value().unwrap(),
                x1
            );

            assert_eq!(
                q.y.get_value().unwrap(),
                y1
            );
        }
    }

    #[test]
    fn test_conditionally_select() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..1000 {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let p = edwards::Point::<Bls12, _>::rand(rng, params);

            let (x0, y0) = p.into_xy();

            let num_x0 = AllocatedNum::alloc(cs.namespace(|| "x0"), || {
                Ok(x0)
            }).unwrap();
            let num_y0 = AllocatedNum::alloc(cs.namespace(|| "y0"), || {
                Ok(y0)
            }).unwrap();

            let p = EdwardsPoint {
                x: num_x0,
                y: num_y0
            };

            let mut should_we_select = rng.gen();

            // Conditionally allocate
            let mut b = if rng.gen() {
                Boolean::from(AllocatedBit::alloc(
                    cs.namespace(|| "condition"),
                    Some(should_we_select)
                ).unwrap())
            } else {
                Boolean::constant(should_we_select)
            };

            // Conditionally negate
            if rng.gen() {
                b = b.not();
                should_we_select = !should_we_select;
            }

            let q = p.conditionally_select(cs.namespace(|| "select"), &b).unwrap();

            assert!(cs.is_satisfied());

            if should_we_select {
                assert_eq!(q.x.get_value().unwrap(), x0);
                assert_eq!(q.y.get_value().unwrap(), y0);

                cs.set("select/y'/num", Fr::one());
                assert_eq!(cs.which_is_unsatisfied().unwrap(), "select/y' computation");
                cs.set("select/x'/num", Fr::zero());
                assert_eq!(cs.which_is_unsatisfied().unwrap(), "select/x' computation");
            } else {
                assert_eq!(q.x.get_value().unwrap(), Fr::zero());
                assert_eq!(q.y.get_value().unwrap(), Fr::one());

                cs.set("select/y'/num", x0);
                assert_eq!(cs.which_is_unsatisfied().unwrap(), "select/y' computation");
                cs.set("select/x'/num", y0);
                assert_eq!(cs.which_is_unsatisfied().unwrap(), "select/x' computation");
            }
        }
    }

    #[test]
    fn test_edwards_addition() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let p1 = edwards::Point::<Bls12, _>::rand(rng, params);
            let p2 = edwards::Point::<Bls12, _>::rand(rng, params);

            let p3 = p1.add(&p2, params);

            let (x0, y0) = p1.into_xy();
            let (x1, y1) = p2.into_xy();
            let (x2, y2) = p3.into_xy();

            let mut cs = TestConstraintSystem::<Bls12>::new();

            let num_x0 = AllocatedNum::alloc(cs.namespace(|| "x0"), || {
                Ok(x0)
            }).unwrap();
            let num_y0 = AllocatedNum::alloc(cs.namespace(|| "y0"), || {
                Ok(y0)
            }).unwrap();

            let num_x1 = AllocatedNum::alloc(cs.namespace(|| "x1"), || {
                Ok(x1)
            }).unwrap();
            let num_y1 = AllocatedNum::alloc(cs.namespace(|| "y1"), || {
                Ok(y1)
            }).unwrap();

            let p1 = EdwardsPoint {
                x: num_x0,
                y: num_y0
            };

            let p2 = EdwardsPoint {
                x: num_x1,
                y: num_y1
            };

            let p3 = p1.add(cs.namespace(|| "addition"), &p2, params).unwrap();

            assert!(cs.is_satisfied());

            assert!(p3.x.get_value().unwrap() == x2);
            assert!(p3.y.get_value().unwrap() == y2);

            let u = cs.get("addition/U/num");
            cs.set("addition/U/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/U computation"));
            cs.set("addition/U/num", u);
            assert!(cs.is_satisfied());

            let x3 = cs.get("addition/x3/num");
            cs.set("addition/x3/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/x3 computation"));
            cs.set("addition/x3/num", x3);
            assert!(cs.is_satisfied());

            let y3 = cs.get("addition/y3/num");
            cs.set("addition/y3/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/y3 computation"));
            cs.set("addition/y3/num", y3);
            assert!(cs.is_satisfied());
        }
    }

    #[test]
    fn test_edwards_doubling() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let p1 = edwards::Point::<Bls12, _>::rand(rng, params);
            let p2 = p1.double(params);

            let (x0, y0) = p1.into_xy();
            let (x1, y1) = p2.into_xy();

            let mut cs = TestConstraintSystem::<Bls12>::new();

            let num_x0 = AllocatedNum::alloc(cs.namespace(|| "x0"), || {
                Ok(x0)
            }).unwrap();
            let num_y0 = AllocatedNum::alloc(cs.namespace(|| "y0"), || {
                Ok(y0)
            }).unwrap();

            let p1 = EdwardsPoint {
                x: num_x0,
                y: num_y0
            };

            let p2 = p1.double(cs.namespace(|| "doubling"), params).unwrap();

            assert!(cs.is_satisfied());

            assert!(p2.x.get_value().unwrap() == x1);
            assert!(p2.y.get_value().unwrap() == y1);
        }
    }

    #[test]
    fn test_montgomery_addition() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let p1 = loop {
                let x: Fr = rng.gen();
                let s: bool = rng.gen();

                if let Some(p) = montgomery::Point::<Bls12, _>::get_for_x(x, s, params) {
                    break p;
                }
            };

            let p2 = loop {
                let x: Fr = rng.gen();
                let s: bool = rng.gen();

                if let Some(p) = montgomery::Point::<Bls12, _>::get_for_x(x, s, params) {
                    break p;
                }
            };

            let p3 = p1.add(&p2, params);

            let (x0, y0) = p1.into_xy().unwrap();
            let (x1, y1) = p2.into_xy().unwrap();
            let (x2, y2) = p3.into_xy().unwrap();

            let mut cs = TestConstraintSystem::<Bls12>::new();

            let num_x0 = AllocatedNum::alloc(cs.namespace(|| "x0"), || {
                Ok(x0)
            }).unwrap();
            let num_y0 = AllocatedNum::alloc(cs.namespace(|| "y0"), || {
                Ok(y0)
            }).unwrap();

            let num_x1 = AllocatedNum::alloc(cs.namespace(|| "x1"), || {
                Ok(x1)
            }).unwrap();
            let num_y1 = AllocatedNum::alloc(cs.namespace(|| "y1"), || {
                Ok(y1)
            }).unwrap();

            let p1 = MontgomeryPoint {
                x: num_x0,
                y: num_y0
            };

            let p2 = MontgomeryPoint {
                x: num_x1,
                y: num_y1
            };

            let p3 = p1.add(cs.namespace(|| "addition"), &p2, params).unwrap();

            assert!(cs.is_satisfied());

            assert!(p3.x.get_value().unwrap() == x2);
            assert!(p3.y.get_value().unwrap() == y2);

            cs.set("addition/yprime/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/evaluate yprime"));
            cs.set("addition/yprime/num", y2);
            assert!(cs.is_satisfied());

            cs.set("addition/xprime/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/evaluate xprime"));
            cs.set("addition/xprime/num", x2);
            assert!(cs.is_satisfied());

            cs.set("addition/lambda/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("addition/evaluate lambda"));
        }
    }

    #[test]
    fn test_montgomery_doubling() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for _ in 0..100 {
            let p = loop {
                let x: Fr = rng.gen();
                let s: bool = rng.gen();

                if let Some(p) = montgomery::Point::<Bls12, _>::get_for_x(x, s, params) {
                    break p;
                }
            };

            let p2 = p.double(params);

            let (x0, y0) = p.into_xy().unwrap();
            let (x1, y1) = p2.into_xy().unwrap();

            let mut cs = TestConstraintSystem::<Bls12>::new();

            let x = AllocatedNum::alloc(cs.namespace(|| "x"), || {
                Ok(x0)
            }).unwrap();
            let y = AllocatedNum::alloc(cs.namespace(|| "y"), || {
                Ok(y0)
            }).unwrap();

            let p = MontgomeryPoint {
                x: x,
                y: y
            };

            let p2 = p.double(cs.namespace(|| "doubling"), params).unwrap();

            assert!(cs.is_satisfied());

            assert!(p2.x.get_value().unwrap() == x1);
            assert!(p2.y.get_value().unwrap() == y1);

            cs.set("doubling/yprime/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("doubling/evaluate yprime"));
            cs.set("doubling/yprime/num", y1);
            assert!(cs.is_satisfied());

            cs.set("doubling/xprime/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("doubling/evaluate xprime"));
            cs.set("doubling/xprime/num", x1);
            assert!(cs.is_satisfied());

            cs.set("doubling/lambda/num", rng.gen());
            assert_eq!(cs.which_is_unsatisfied(), Some("doubling/evaluate lambda"));
        }
    }
}
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Matrix {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) e: f64,
    pub(crate) f: f64,
}

impl Matrix {
    pub(crate) const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub(crate) fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Result<Self> {
        let matrix = Self { a, b, c, d, e, f };
        if matrix.is_finite() {
            Ok(matrix)
        } else {
            Err(Error::Unresolved(
                "transformation matrix contains a non-finite value".to_owned(),
            ))
        }
    }

    pub(crate) fn translation(x: f64, y: f64) -> Result<Self> {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    pub(crate) fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .into_iter()
            .all(f64::is_finite)
    }

    /// Concatenates a PDF matrix operand with the current matrix.
    pub(crate) fn concatenate(self, operand: Self) -> Result<Self> {
        Self::new(
            self.a * operand.a + self.c * operand.b,
            self.b * operand.a + self.d * operand.b,
            self.a * operand.c + self.c * operand.d,
            self.b * operand.c + self.d * operand.d,
            self.a * operand.e + self.c * operand.f + self.e,
            self.b * operand.e + self.d * operand.f + self.f,
        )
    }

    pub(crate) fn transform_point(self, x: f64, y: f64) -> Result<(f64, f64)> {
        finite_pair(
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub(crate) fn transform_vector(self, x: f64, y: f64) -> Result<(f64, f64)> {
        finite_pair(self.a * x + self.c * y, self.b * x + self.d * y)
    }
}

fn finite_pair(x: f64, y: f64) -> Result<(f64, f64)> {
    if x.is_finite() && y.is_finite() {
        Ok((x, y))
    } else {
        Err(Error::Unresolved(
            "matrix transformation produced a non-finite value".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenates_in_pdf_order() -> Result<()> {
        let scale = Matrix::new(2.0, 0.0, 0.0, 3.0, 0.0, 0.0)?;
        let translated = scale.concatenate(Matrix::translation(5.0, 7.0)?)?;

        assert_eq!(translated.transform_point(1.0, 1.0)?, (12.0, 24.0));
        assert_eq!(translated.transform_vector(1.0, 1.0)?, (2.0, 3.0));
        Ok(())
    }

    #[test]
    fn rejects_non_finite_inputs_and_results() -> Result<()> {
        assert!(Matrix::translation(f64::INFINITY, 0.0).is_err());
        let huge = Matrix::new(f64::MAX, 0.0, 0.0, 1.0, 0.0, 0.0)?;
        assert!(huge.concatenate(huge).is_err());
        assert!(Matrix::IDENTITY.transform_point(f64::NAN, 0.0).is_err());
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FracCounter {
    acc: f64,
}

impl FracCounter {
    pub(crate) fn take(&mut self, rate: f64) -> usize {
        self.acc += rate.max(0.0);
        let whole = self.acc.floor();
        self.acc -= whole;
        whole as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_fractional_rate() {
        let mut c = FracCounter::default();
        let taken: Vec<_> = (0..4).map(|_| c.take(0.5)).collect();
        assert_eq!(taken, [0, 1, 0, 1]);
        assert_eq!(c.take(3.0), 3);
    }
}

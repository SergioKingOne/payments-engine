//! Money types: a validated transaction [`Amount`] and an account's [`Balances`].
//!
//! All arithmetic is checked. Inputs are bounded by [`Amount::MAX`], so balances stay far below
//! the magnitude where `Decimal` would start rounding instead of reporting overflow.

use std::str::FromStr;

use rust_decimal::Decimal;

/// Decimal places accepted on input and written on output.
pub const PRECISION: u32 = 4;

/// A transaction amount: strictly positive, at most [`PRECISION`] decimal places, at most
/// [`Amount::MAX`]. The only ways to build one are [`Amount::new`] and [`str::parse`], so holding
/// an `Amount` is proof that it is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(Decimal);

impl Amount {
    /// The largest accepted amount, 10^14. With at most 2^32 transactions, no balance can exceed
    /// about 4.3e23, well inside the range where 4-decimal-place arithmetic is exact (~7.9e24).
    pub const MAX: Decimal = Decimal::from_parts(0x107A_4000, 0x5AF3, 0, false, 0);

    /// Validates `value` as a transaction amount. Trailing zeros are ignored, so `1.50000` is
    /// accepted as `1.5`.
    ///
    /// # Errors
    ///
    /// Returns an [`AmountError`] if `value` is zero or negative, has more than [`PRECISION`]
    /// significant decimal places, or exceeds [`Amount::MAX`].
    pub fn new(value: Decimal) -> Result<Self, AmountError> {
        let value = value.normalize();
        if value <= Decimal::ZERO {
            return Err(AmountError::NotPositive);
        }
        if value.scale() > PRECISION {
            return Err(AmountError::TooPrecise {
                scale: value.scale(),
            });
        }
        if value > Self::MAX {
            return Err(AmountError::TooLarge);
        }
        Ok(Self(value))
    }

    /// The validated value.
    #[must_use]
    pub fn value(self) -> Decimal {
        self.0
    }
}

impl FromStr for Amount {
    type Err = AmountError;

    /// Parses exactly: an optional sign, digits and at most one decimal point. Never rounds, and
    /// rejects what `Decimal`'s own parsers tolerate: scientific notation, and `_` between
    /// digits (`1__0` would otherwise read as 10).
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
        let well_formed = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && digits.bytes().filter(|&b| b == b'.').count() <= 1;
        let value = well_formed
            .then(|| Decimal::from_str_exact(raw).ok())
            .flatten()
            .ok_or_else(|| AmountError::Invalid(excerpt(raw)))?;
        Self::new(value)
    }
}

/// At most the first 32 characters of `raw`, so a huge junk field cannot flood the diagnostics.
fn excerpt(raw: &str) -> String {
    const MAX_CHARS: usize = 32;
    match raw.char_indices().nth(MAX_CHARS) {
        Some((end, _)) => format!("{}...", raw.get(..end).unwrap_or_default()),
        None => raw.to_owned(),
    }
}

/// Why a value is not a valid [`Amount`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    #[error("{0:?} is not a decimal number")]
    Invalid(String),
    #[error("amount must be positive")]
    NotPositive,
    #[error("amount has {scale} decimal places; at most {PRECISION} are allowed")]
    TooPrecise { scale: u32 },
    #[error("amount exceeds the maximum of {}", Amount::MAX)]
    TooLarge,
}

/// An account's funds. `total` is stored rather than derived, and the only constructor computes
/// it, so `total == available + held` holds for every value of this type.
///
/// Operations are pure: each returns the new balances, or `None` on arithmetic overflow, and
/// never changes `self`. Callers commit the result only when the whole operation succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Balances {
    available: Decimal,
    held: Decimal,
    total: Decimal,
}

impl Balances {
    /// Builds balances from their parts, or `None` if the total overflows.
    #[must_use]
    pub fn new(available: Decimal, held: Decimal) -> Option<Self> {
        let total = available.checked_add(held)?;
        Some(Self {
            available,
            held,
            total,
        })
    }

    /// Funds free to withdraw. Negative after a dispute on funds that were already withdrawn.
    #[must_use]
    pub fn available(self) -> Decimal {
        self.available
    }

    /// Funds held by open disputes.
    #[must_use]
    pub fn held(self) -> Decimal {
        self.held
    }

    /// `available + held`.
    #[must_use]
    pub fn total(self) -> Decimal {
        self.total
    }

    /// Credits available funds.
    #[must_use]
    pub fn deposit(self, amount: Amount) -> Option<Self> {
        Self::new(self.available.checked_add(amount.0)?, self.held)
    }

    /// Debits available funds. Does not check sufficiency: that is a business rule, not arithmetic.
    #[must_use]
    pub fn withdraw(self, amount: Amount) -> Option<Self> {
        Self::new(self.available.checked_sub(amount.0)?, self.held)
    }

    /// Moves disputed funds from available to held. Total is unchanged.
    #[must_use]
    pub fn hold(self, amount: Amount) -> Option<Self> {
        Self::new(
            self.available.checked_sub(amount.0)?,
            self.held.checked_add(amount.0)?,
        )
    }

    /// Returns held funds to available after a resolved dispute. Total is unchanged.
    #[must_use]
    pub fn release(self, amount: Amount) -> Option<Self> {
        Self::new(
            self.available.checked_add(amount.0)?,
            self.held.checked_sub(amount.0)?,
        )
    }

    /// Removes held funds after a chargeback. Held and total both decrease.
    #[must_use]
    pub fn charge_back(self, amount: Amount) -> Option<Self> {
        Self::new(self.available, self.held.checked_sub(amount.0)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(raw: &str) -> Decimal {
        Decimal::from_str_exact(raw).unwrap()
    }

    fn amount(raw: &str) -> Amount {
        raw.parse().unwrap()
    }

    fn balances(available: &str, held: &str) -> Balances {
        Balances::new(dec(available), dec(held)).unwrap()
    }

    #[test]
    fn amount_max_is_ten_to_the_fourteenth() {
        assert_eq!(Amount::MAX, Decimal::from(100_000_000_000_000_u64));
    }

    #[test]
    fn amount_accepts_up_to_four_decimal_places() {
        assert_eq!(amount("1").value(), dec("1"));
        assert_eq!(amount("2.5").value(), dec("2.5"));
        assert_eq!(amount("0.0001").value(), dec("0.0001"));
        assert_eq!(amount("1.2345").value(), dec("1.2345"));
    }

    #[test]
    fn amount_accepts_trailing_zeros_beyond_four_places() {
        assert_eq!(amount("1.500000").value(), dec("1.5"));
    }

    #[test]
    fn amount_rejects_more_than_four_decimal_places() {
        assert_eq!(
            "1.23456".parse::<Amount>(),
            Err(AmountError::TooPrecise { scale: 5 })
        );
    }

    #[test]
    fn amount_rejects_zero_and_negative() {
        assert_eq!("0".parse::<Amount>(), Err(AmountError::NotPositive));
        assert_eq!("0.0000".parse::<Amount>(), Err(AmountError::NotPositive));
        assert_eq!("-1.5".parse::<Amount>(), Err(AmountError::NotPositive));
    }

    #[test]
    fn amount_rejects_above_max() {
        assert_eq!(amount("100000000000000").value(), Amount::MAX);
        assert_eq!(
            "100000000000000.0001".parse::<Amount>(),
            Err(AmountError::TooLarge)
        );
    }

    #[test]
    fn amount_rejects_scientific_notation() {
        assert_eq!(
            "1e3".parse::<Amount>(),
            Err(AmountError::Invalid("1e3".into()))
        );
    }

    #[test]
    fn amount_rejects_non_numbers() {
        for raw in [
            "", "abc", "1.2.3", "1,5", "1_000", "1__0", "_1", "0x10", "+", ".", "--1", " 1",
        ] {
            assert_eq!(
                raw.parse::<Amount>(),
                Err(AmountError::Invalid(raw.into())),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn amount_accepts_sign_and_bare_decimal_point() {
        assert_eq!(amount("+2").value(), dec("2"));
        assert_eq!(amount(".5").value(), dec("0.5"));
        assert_eq!(amount("3.").value(), dec("3"));
    }

    #[test]
    fn invalid_amount_is_quoted_in_part() {
        let raw = "9".repeat(10_000) + "x";
        let Err(AmountError::Invalid(quoted)) = raw.parse::<Amount>() else {
            panic!("expected Invalid");
        };
        assert_eq!(quoted, "9".repeat(32) + "...");
    }

    #[test]
    fn amount_parse_never_rounds() {
        // 29 decimal places: `Decimal::from_str` would silently round this to 1.
        let raw = "1.00000000000000000000000000001";
        assert!(raw.parse::<Amount>().is_err());
    }

    #[test]
    fn balances_total_equals_available_plus_held() {
        let b = balances("-1.5", "4.25");
        assert_eq!(b.total(), dec("2.75"));
    }

    #[test]
    fn balances_overflow_returns_none() {
        assert_eq!(Balances::new(Decimal::MAX, Decimal::ONE), None);
    }

    #[test]
    fn deposit_credits_available_and_total() {
        let b = balances("1", "2").deposit(amount("0.5")).unwrap();
        assert_eq!(b, balances("1.5", "2"));
        assert_eq!(b.total(), dec("3.5"));
    }

    #[test]
    fn withdraw_debits_available_and_total() {
        let b = balances("3", "2").withdraw(amount("0.5")).unwrap();
        assert_eq!(b, balances("2.5", "2"));
        assert_eq!(b.total(), dec("4.5"));
    }

    #[test]
    fn hold_moves_funds_to_held_and_keeps_total() {
        let before = balances("3", "0");
        let after = before.hold(amount("2")).unwrap();
        assert_eq!(after, balances("1", "2"));
        assert_eq!(after.total(), before.total());
    }

    #[test]
    fn hold_may_drive_available_negative() {
        let after = balances("1", "0").hold(amount("3")).unwrap();
        assert_eq!(after, balances("-2", "3"));
    }

    #[test]
    fn release_undoes_hold() {
        let before = balances("3", "1");
        let held = before.hold(amount("2")).unwrap();
        assert_eq!(held.release(amount("2")).unwrap(), before);
    }

    #[test]
    fn charge_back_reduces_held_and_total() {
        let after = balances("1", "2").charge_back(amount("2")).unwrap();
        assert_eq!(after, balances("1", "0"));
        assert_eq!(after.total(), dec("1"));
    }
}

"""Demo billing module for CodeRadar self-review mutation tests.

Contains deliberate bug(s) targeted by coderadar_replace_body /
coderadar_update_signature / coderadar_rename / coderadar_create_entity.
"""
from __future__ import annotations


def load_rates(currency: str) -> float:
    """Return the conversion multiplier for a currency."""
    rates = {"USD": 1.0, "EUR": 1.08, "GBP": 1.27}
    return rates.get(currency, 1.0)


class Invoice:
    """A demo invoice that aggregates line items."""

    def __init__(self, customer: str, tax_rate: float = 0.0):
        self.customer = customer
        self.tax_rate = tax_rate
        self.items: list[tuple[str, float, int]] = []

    def add_item(self, label: str, price: float, quantity: int = 1) -> None:
        """Append a line item (price is per unit)."""
        self.items.append((label, price, quantity))

    def calculate_total(self) -> float:
        """BUG: applies discount before tax, so tax is computed on the net.

        The intended behaviour is tax-then-discount. Kept short so a
        replace_body mutation has a small, obvious target.
        """
        subtotal = sum(price * qty for _, price, qty in self.items)
        discounted = subtotal * 0.9
        return discounted * (1 + self.tax_rate)

    def apply_loyalty_discount(self, pct: float, tier: str) -> float:
        """Target for update_signature: tier is unused, rate is duplicated."""
        if tier == "gold":
            pct = pct + 5.0
        if tier == "silver":
            pct = pct + 2.0
        return pct

    def summary(self) -> str:
        """Renders the invoice; calls calculate_total."""
        total = self.calculate_total()
        return f"Invoice for {self.customer}: ${total:.2f} ({len(self.items)} items)"


def build_demo_invoice(customer: str) -> Invoice:
    """Factory used by demo scripts; exercises the whole chain."""
    inv = Invoice(customer)
    inv.add_item("Widget", 19.99, 2)
    inv.add_item("Gizmo", 4.5, 3)
    return inv


def print_invoice(customer: str, currency: str) -> str:
    """Top-level entry point; calls build_demo_invoice and load_rates."""
    inv = build_demo_invoice(customer)
    rate = load_rates(currency)
    return f"[{currency} x{rate}] {inv.summary()}"

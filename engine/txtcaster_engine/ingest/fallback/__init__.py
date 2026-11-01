"""ING-FALLBACK: cost-first managed vision cascade for scanned pages and high-entropy tables.

Owner rulings #36.1 and #40: the cascade fires only on ING-2's `needs_fallback` markers
`scanned` and `high_entropy_table`, never on a prose page; providers are ordered strictly by
unit cost (OpenAI gpt-5.6-luna, Google gemini-3.5-flash-lite, Google gemini-3.8-flash); every
call is metered into a per-book ledger; the desktop hands the user's own key over the existing
bearer channel and the sidecar calls the provider directly. Prices and formulas come from the
survey of record (fallback-provider-survey-20260905.md §1, §2, §5, §6) and are not re-derived.
"""

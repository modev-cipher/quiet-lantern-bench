"""Provider adapters: one request builder and one response parser per model id, behind a
`Wire` callable so tests replay scripted bodies and only the opt-in live smoke opens a socket."""

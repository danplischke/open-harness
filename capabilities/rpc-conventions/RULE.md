# RPC service conventions

- Validate every request at the service boundary before touching state.
- Return typed errors; never leak internal error strings to clients.
- Keep handlers thin — push business logic into the domain layer.

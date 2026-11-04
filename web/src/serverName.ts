// The same rule as `is_valid_server_name` in crates/core/src/server_name.rs.
const SERVER_NAME = /^[a-z0-9][a-z0-9-]{0,62}$/
// The same list as `RESERVED_SERVER_NAMES`. These names are also fixed API routes.
const RESERVED = ['with-tool']

export const isValidServerName = (name: string) => SERVER_NAME.test(name) && !RESERVED.includes(name)

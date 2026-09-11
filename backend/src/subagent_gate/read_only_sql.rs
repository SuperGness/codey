//! Conservative lexical read-only checks for database MCP tools. The lexer is
//! a denylist over SQL tokens, not a parser: it must still be paired with a
//! read-only database account.

use serde_json::Value;

pub(super) fn database_mcp_is_read_only(tool_name: &str, tool_input: Option<&Value>) -> bool {
    let Some((server, tool)) = tool_name
        .strip_prefix("mcp__")
        .and_then(|name| name.rsplit_once("__"))
        .filter(|(server, tool)| !server.is_empty() && !tool.is_empty())
    else {
        return false;
    };

    if matches!(
        tool,
        "get_schema"
            | "get_db_schema"
            | "get_database_schema"
            | "get_schema_info"
            | "get_table_schema"
            | "get_table_definition"
            | "get_table_ddl"
            | "get_connection"
            | "get_connection_status"
            | "connection_status"
            | "test_connection"
            | "ping"
            | "health_check"
            | "get_database_info"
            | "get_server_info"
            | "get_table_info"
            | "describe_table"
            | "describe_tables"
            | "inspect_schema"
            | "inspect_table"
            | "list_databases"
            | "list_schemas"
            | "list_tables"
            | "list_columns"
            | "show_databases"
            | "show_schemas"
            | "show_tables"
            | "show_columns"
            | "show_create_table"
    ) {
        return tool_input.is_none_or(|value| value.is_null() || value.is_object());
    }

    let sql_tool = matches!(
        tool,
        "execute_sql" | "run_sql" | "sql_query" | "query_sql" | "read_query"
    ) || (matches!(
        tool,
        "query" | "execute_query" | "run_query" | "query_database"
    ) && database_server_name(server));
    sql_tool && sql_input(tool_input).is_some_and(sql_is_read_only)
}

fn database_server_name(server: &str) -> bool {
    server
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| {
            matches!(
                part,
                "db" | "database"
                    | "sql"
                    | "mysql"
                    | "mariadb"
                    | "postgres"
                    | "postgresql"
                    | "sqlite"
                    | "duckdb"
                    | "mssql"
                    | "oracle"
                    | "clickhouse"
                    | "snowflake"
                    | "bigquery"
                    | "supabase"
            )
        })
        || server.ends_with("db")
}

fn sql_input(tool_input: Option<&Value>) -> Option<&str> {
    let input = tool_input?.as_object()?;
    let mut sql = None;
    for key in ["sql", "query", "statement", "sql_query"] {
        let Some(value) = input.get(key) else {
            continue;
        };
        let candidate = value.as_str()?.trim();
        if candidate.is_empty() || sql.is_some_and(|current| current != candidate) {
            return None;
        }
        sql = Some(candidate);
    }
    sql
}

pub(super) fn sql_is_read_only(sql: &str) -> bool {
    let Some(mut tokens) = sql_tokens(sql) else {
        return false;
    };
    if tokens.last().is_some_and(|token| token == ";") {
        tokens.pop();
    }
    if tokens.is_empty() || tokens.iter().any(|token| token == ";") {
        return false;
    }

    let first = tokens[0].as_str();
    let forbidden = |token: &str| {
        matches!(
            token,
            "INSERT"
                | "UPDATE"
                | "DELETE"
                | "REPLACE"
                | "UPSERT"
                | "MERGE"
                | "CREATE"
                | "ALTER"
                | "DROP"
                | "TRUNCATE"
                | "RENAME"
                | "GRANT"
                | "REVOKE"
                | "COMMENT"
                | "CALL"
                | "EXEC"
                | "EXECUTE"
                | "DO"
                | "SET"
                | "RESET"
                | "USE"
                | "ATTACH"
                | "DETACH"
                | "COPY"
                | "LOAD"
                | "LOCK"
                | "UNLOCK"
                | "VACUUM"
                | "ANALYZE"
                | "CLUSTER"
                | "REINDEX"
                | "REFRESH"
                | "BEGIN"
                | "START"
                | "COMMIT"
                | "ROLLBACK"
                | "SAVEPOINT"
                | "RELEASE"
                | "PREPARE"
                | "DEALLOCATE"
                | "DISCARD"
                | "LISTEN"
                | "NOTIFY"
                | "UNLISTEN"
                | "SECURITY"
                | "CHECKPOINT"
                | "SHUTDOWN"
                | "KILL"
                | "OPTIMIZE"
                | "REPAIR"
                | "INSTALL"
                | "UNINSTALL"
                | "INTO"
                | "OUTFILE"
                | "DUMPFILE"
                | "NEXTVAL"
                | "SETVAL"
                | "SET_CONFIG"
                | "GET_LOCK"
                | "RELEASE_LOCK"
                | "PG_ADVISORY_LOCK"
                | "PG_ADVISORY_XACT_LOCK"
                | "PG_ADVISORY_UNLOCK"
                | "PG_ADVISORY_UNLOCK_ALL"
                | "PG_CANCEL_BACKEND"
                | "PG_TERMINATE_BACKEND"
                | "PG_RELOAD_CONF"
                | "PG_ROTATE_LOGFILE"
                | "DBLINK_EXEC"
                | "DBLINK"
                | "LOAD_FILE"
                | "PG_READ_FILE"
                | "PG_READ_BINARY_FILE"
                | "PG_LS_DIR"
                | "PG_SLEEP"
                | "PG_SLEEP_FOR"
                | "PG_SLEEP_UNTIL"
                | "SLEEP"
                | "BENCHMARK"
                | "OPENROWSET"
                | "OPENDATASOURCE"
                | "LO_EXPORT"
                | "LO_IMPORT"
                | "LO_UNLINK"
        ) || token.starts_with("XP_")
    };

    match first {
        "SHOW" => !tokens
            .iter()
            .enumerate()
            .any(|(index, token)| forbidden(token) && !(index == 1 && token == "CREATE")),
        "DESCRIBE" | "DESC" | "SELECT" => !tokens.iter().any(|token| forbidden(token)),
        "WITH" => {
            tokens.iter().any(|token| token == "SELECT")
                && !tokens.iter().any(|token| forbidden(token))
        }
        "EXPLAIN" => {
            tokens.iter().skip(1).any(|token| {
                matches!(
                    token.as_str(),
                    "SELECT" | "WITH" | "SHOW" | "DESCRIBE" | "DESC"
                )
            }) && !tokens.iter().any(|token| forbidden(token))
        }
        _ => false,
    }
}

fn sql_tokens(sql: &str) -> Option<Vec<String>> {
    // ponytail: lexical SQL checks cannot prove user-defined functions are pure; keep database
    // credentials read-only until Hook payloads expose trusted connector capability metadata.
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                // MySQL requires whitespace after --; other dialects do not.
                if bytes
                    .get(index + 2)
                    .is_some_and(|byte| !byte.is_ascii_whitespace())
                {
                    return None;
                }
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'#' | b'[' | b']' | b'$' | b'\\' => return None,
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                if matches!(bytes.get(index + 2), Some(b'!'))
                    || matches!(
                        (bytes.get(index + 2), bytes.get(index + 3)),
                        (Some(b'M' | b'm'), Some(b'!'))
                    )
                {
                    return None;
                }
                index += 2;
                let mut depth = 1;
                while index < bytes.len() && depth > 0 {
                    if bytes.get(index..index + 2) == Some(b"/*") {
                        // Nested comments end at different positions across dialects.
                        return None;
                    } else if bytes.get(index..index + 2) == Some(b"*/") {
                        depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                if depth != 0 {
                    return None;
                }
            }
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                let start = index;
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        return None;
                    } else if bytes[index] == quote {
                        if bytes.get(index + 1) == Some(&quote) {
                            index += 2;
                        } else {
                            index += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
                if !closed {
                    return None;
                }
                if quote != b'\'' {
                    tokens.push(sql[start..index - 1].to_ascii_uppercase());
                }
            }
            b';' => {
                tokens.push(";".to_string());
                index += 1;
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                let token = sql[start..index].to_ascii_uppercase();
                if token == "E" && bytes.get(index) == Some(&b'\'') {
                    return None;
                }
                tokens.push(token);
            }
            _ => index += 1,
        }
    }
    Some(tokens)
}

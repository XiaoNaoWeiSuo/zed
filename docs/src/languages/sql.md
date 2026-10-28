---
title: SQL
description: "Configure SQL language support in Lin, including language servers, formatting, and debugging."
---

# SQL

SQL files are handled by the [SQL Extension](https://github.com/lin-extensions/sql).

- Tree-sitter: [nervenes/tree-sitter-sql](https://github.com/nervenes/tree-sitter-sql)

### Formatting

Lin supports auto-formatting SQL using external tools like [`sql-formatter`](https://github.com/sql-formatter-org/sql-formatter).

1. Install `sql-formatter`:

```sh
npm install -g sql-formatter
```

2. Ensure `sql-formatter` is available in your path and check the version:

```sh
which sql-formatter
sql-formatter --version
```

3. Configure formatting in Settings ({#kb lin::OpenSettings}) under Languages > SQL, or add to your settings file:

```json [settings]
  "languages": {
    "SQL": {
      "formatter": {
        "external": {
          "command": "sql-formatter",
          "arguments": ["--language", "mysql"]
        }
      }
    }
  },
```

Substitute your preferred [SQL Dialect] for `mysql` above (`duckdb`, `hive`, `mariadb`, `postgresql`, `redshift`, `snowflake`, `sqlite`, `spark`, etc).

You can add this to Lin project settings (`.lin/settings.json`) or via your Lin user settings (`~/.config/lin/settings.json`).

### Advanced Formatting

Sql-formatter also allows more precise control by providing [sql-formatter configuration options](https://github.com/sql-formatter-org/sql-formatter#configuration-options). To provide these, create a `.sql-formatter.json` file in your project:

```json
{
  "language": "postgresql",
  "tabWidth": 2,
  "keywordCase": "upper",
  "linesBetweenQueries": 2
}
```

When using a `.sql-formatter.json` file you can use a simplified Lin settings configuration:

```json [settings]
{
  "languages": {
    "SQL": {
      "formatter": {
        "external": {
          "command": "sql-formatter"
        }
      }
    }
  }
}
```

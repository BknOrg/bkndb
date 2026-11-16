/*
 * BknDb - Embedded hybrid graph-relational database engine with ACID transactions
 * Public C-ABI Header (bkndb.h)
 */

#ifndef BKNDB_H
#define BKNDB_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque database engine handle */
typedef struct BknDbEngine BknDbEngine;

/* Opaque transaction handle */
typedef struct BknDbTransaction BknDbTransaction;

/* -------------------------------------------------------------------------
 * Memory Management
 * ------------------------------------------------------------------------- */

/**
 * Frees a C string returned by BknDb.
 */
void bkndb_free_string(char *s);

/**
 * Closes (if not already closed) and frees a BknDbEngine instance.
 */
void bkndb_free_engine(BknDbEngine *engine);

/**
 * Frees a BknDbTransaction instance.
 */
void bkndb_free_transaction(BknDbTransaction *tx);

/* -------------------------------------------------------------------------
 * Lifecycle & Maintenance
 * ------------------------------------------------------------------------- */

/**
 * Opens or creates a persistent single-file database at `path`.
 * Returns NULL on error and populates `err_out`.
 */
BknDbEngine *bkndb_open(const char *path, char **err_out);

/**
 * Creates an in-memory database instance.
 */
BknDbEngine *bkndb_in_memory(char **err_out);

/**
 * Closes the database, releasing any file locks.
 * Returns 0 on success, -1 on error.
 */
int32_t bkndb_close(BknDbEngine *engine, char **err_out);

/**
 * Returns true if the database engine handle is closed.
 */
bool bkndb_is_closed(BknDbEngine *engine);

/**
 * Reclaims disk space and compacts on-disk storage.
 */
int32_t bkndb_compact(BknDbEngine *engine, char **err_out);

/**
 * Backs up the database to `dest`.
 */
int32_t bkndb_backup(BknDbEngine *engine, const char *dest, char **err_out);

/**
 * Returns database statistics as a JSON string.
 */
char *bkndb_stats(BknDbEngine *engine, char **err_out);

/* -------------------------------------------------------------------------
 * SQL & Graph Queries
 * ------------------------------------------------------------------------- */

/**
 * Executes a SQL statement. `params_json` can be NULL, an array, or an object.
 * Returns a JSON string with format {"columns": [...], "rows": [...], "affected": N}.
 */
char *bkndb_sql(BknDbEngine *engine, const char *query, const char *params_json, char **err_out);

/**
 * Executes a Cypher-like MATCH ... RETURN graph query.
 * Returns a JSON string with format {"columns": [...], "rows": [...], "affected": N}.
 */
char *bkndb_graph_query(BknDbEngine *engine, const char *query, const char *params_json, char **err_out);

/* -------------------------------------------------------------------------
 * Graph Operations
 * ------------------------------------------------------------------------- */

/**
 * Creates a graph node with a label and properties JSON object string.
 * Writes the new node ID to `out_id`. Returns 0 on success.
 */
int32_t bkndb_create_node(BknDbEngine *engine, const char *label, const char *props_json, uint64_t *out_id, char **err_out);

/**
 * Retrieves a node by ID. Returns JSON string {"id": N, "label": "...", "properties": {...}} or "null".
 */
char *bkndb_get_node(BknDbEngine *engine, uint64_t id, char **err_out);

/**
 * Deletes a node and all its connected edges.
 */
int32_t bkndb_delete_node(BknDbEngine *engine, uint64_t id, char **err_out);

/**
 * Updates node properties. `set_props_json` is an object, `unset_keys_json` is an array of string keys.
 */
int32_t bkndb_update_node(BknDbEngine *engine, uint64_t id, const char *set_props_json, const char *unset_keys_json, char **err_out);

/**
 * Creates a directed edge from `from` to `to` with `edge_type` and optional `props_json`.
 * Writes the new edge ID to `out_id`.
 */
int32_t bkndb_create_edge(BknDbEngine *engine, uint64_t from, const char *edge_type, uint64_t to, const char *props_json, uint64_t *out_id, char **err_out);

/**
 * Retrieves an edge by ID. Returns JSON string {"id": N, "from": N, "to": N, "edge_type": "...", "properties": {...}} or "null".
 */
char *bkndb_get_edge(BknDbEngine *engine, uint64_t id, char **err_out);

/**
 * Deletes an edge by ID. Writes true/false to `out_deleted`.
 */
int32_t bkndb_delete_edge(BknDbEngine *engine, uint64_t id, bool *out_deleted, char **err_out);

/**
 * Traverses neighbors of `node`.
 * direction: 0 = Out, 1 = In, 2 = Both.
 * `edge_type`: filter by edge type or NULL for all.
 * Returns JSON array of [{"node_id": N, "edge_id": N, "edge_type": "..."}].
 */
char *bkndb_neighbors(BknDbEngine *engine, uint64_t node, int32_t direction, const char *edge_type, char **err_out);

/* -------------------------------------------------------------------------
 * Search (Full-Text BM25 & Vector HNSW)
 * ------------------------------------------------------------------------- */

/**
 * Creates a BM25 full-text index on `table(column)`.
 */
int32_t bkndb_create_fulltext_index(BknDbEngine *engine, const char *table, const char *column, char **err_out);

/**
 * Performs BM25 full-text search.
 * Returns JSON array of scored rows: [{"pk": ..., "values": {...}, "score": 1.23}].
 */
char *bkndb_search_text(BknDbEngine *engine, const char *table, const char *column, const char *query, uint32_t limit, bool match_all, char **err_out);

/**
 * Creates an approximate (HNSW) vector index on `table(column)`.
 * metric: "cosine", "euclidean", or "dot".
 */
int32_t bkndb_create_vector_index(BknDbEngine *engine, const char *table, const char *column, const char *metric, uint32_t m, uint32_t ef_construction, char **err_out);

/**
 * Performs vector similarity search.
 * Returns JSON array of scored rows: [{"pk": ..., "values": {...}, "score": 0.95}].
 */
char *bkndb_search_vector(BknDbEngine *engine, const char *table, const char *column, const float *vector_ptr, uint32_t vector_len, uint32_t limit, const char *metric, bool exact, char **err_out);

/* -------------------------------------------------------------------------
 * Explicit Transactions
 * ------------------------------------------------------------------------- */

/**
 * Begins an explicit ACID write transaction.
 */
BknDbTransaction *bkndb_begin_transaction(BknDbEngine *engine, char **err_out);

/**
 * Runs a SQL statement within an open transaction.
 */
char *bkndb_tx_sql(BknDbTransaction *tx, const char *query, const char *params_json, char **err_out);

/**
 * Commits the transaction.
 */
int32_t bkndb_tx_commit(BknDbTransaction *tx, char **err_out);

/**
 * Rolls back the transaction.
 */
int32_t bkndb_tx_rollback(BknDbTransaction *tx, char **err_out);

#ifdef __cplusplus
}
#endif

#endif /* BKNDB_H */

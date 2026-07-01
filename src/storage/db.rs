use sqlx::PgPool;
use crate::{error::AppError, models::{Gif, GifRendition}};

const GIF_COLS: &str =
    "id, filename, hash, uploader_id, visibility, is_nsfw, frame_count, duration_ms, uses, uploaded_at";

/// SQL fragment restricting visible GIFs. When `mine` is set, only the
/// viewer's own uploads; otherwise shared GIFs plus the viewer's own.
/// `alias` is the gifs table alias, `param` the bind placeholder for the
/// viewer mxid.
fn visibility_clause(mine: bool, alias: &str, param: &str) -> String {
    if mine {
        format!("{alias}.uploader_id = {param}")
    } else {
        format!("({alias}.visibility = 'shared' OR {alias}.uploader_id = {param})")
    }
}

/// Visibility predicate plus, unless `grab_nsfw`, exclusion of NSFW GIFs.
fn list_filters(mine: bool, grab_nsfw: bool, alias: &str, param: &str) -> String {
    let mut clause = visibility_clause(mine, alias, param);
    if !grab_nsfw {
        clause.push_str(&format!(" AND NOT {alias}.is_nsfw"));
    }
    clause
}

/// SQL fragment (a leading ` AND ...`) excluding GIFs the viewer has hidden,
/// unless `grab_hidden`. `alias` is the gifs table alias, `viewer_param` the
/// bind placeholder for the viewer mxid (reused, no extra bind).
fn hidden_clause(grab_hidden: bool, alias: &str, viewer_param: &str) -> String {
    if grab_hidden {
        String::new()
    } else {
        format!(
            " AND NOT EXISTS (SELECT 1 FROM hidden h \
             WHERE h.gif_id = {alias}.id AND h.mxid = {viewer_param})"
        )
    }
}

pub async fn get_gif(pool: &PgPool, id: &str) -> Result<Option<Gif>, AppError> {
    Ok(sqlx::query_as::<_, Gif>(
        &format!("SELECT {GIF_COLS} FROM gifs WHERE id = $1")
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Dedup lookup, scoped to the uploader so one user can't probe another
/// user's uploads.
pub async fn get_gif_by_hash_for_uploader(
    pool: &PgPool,
    hash: &str,
    uploader_id: &str,
) -> Result<Option<Gif>, AppError> {
    Ok(sqlx::query_as::<_, Gif>(
        &format!("SELECT {GIF_COLS} FROM gifs WHERE uploader_id = $1 AND hash = $2")
    )
    .bind(uploader_id).bind(hash)
    .fetch_optional(pool)
    .await?)
}

pub struct RenditionSpec {
    pub rendition:  &'static str,
    pub filename:   String,
    pub width:      i32,
    pub height:     i32,
    pub size_bytes: i32,
}

pub enum UploadOutcome {
    Inserted(Gif),
    Duplicate(Gif),
    QuotaExceeded,
}

pub enum ReplaceOutcome {
    Updated(Gif),
    /// The new content hashes to a GIF this uploader already owns.
    Duplicate,
    QuotaExceeded,
}

/// Fixed advisory-lock id; all uploads serialize their accounting on it.
const UPLOAD_LOCK_ID: i64 = 0x6749_4653;

/// Atomically check the global and per-user storage caps and, if both pass,
/// insert the GIF + renditions + tags. The cap check and insert run inside a
/// transaction holding a session advisory lock so concurrent uploads can't
/// race past the cap (no TOCTOU). Returns without inserting if the uploader
/// already has this exact file (`Duplicate`) or a cap would be exceeded
/// (`QuotaExceeded`); the transaction rolls back and releases the lock.
#[allow(clippy::too_many_arguments)]
pub async fn insert_gif_quota(
    pool: &PgPool,
    id: &str,
    filename: &str,
    hash: &str,
    uploader_id: &str,
    visibility: &str,
    is_nsfw: bool,
    frame_count: i32,
    duration_ms: i32,
    renditions: &[RenditionSpec],
    tags: &[String],
    global_max: u64,
    per_user_max: u64,
) -> Result<UploadOutcome, AppError> {
    let incoming: i64 = renditions.iter().map(|r| r.size_bytes as i64).sum();
    let mut tx = pool.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(UPLOAD_LOCK_ID)
        .execute(&mut *tx)
        .await?;

    if let Some(existing) = sqlx::query_as::<_, Gif>(
        &format!("SELECT {GIF_COLS} FROM gifs WHERE uploader_id = $1 AND hash = $2")
    )
    .bind(uploader_id).bind(hash)
    .fetch_optional(&mut *tx)
    .await? {
        return Ok(UploadOutcome::Duplicate(existing));
    }

    let (global_total,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(size_bytes),0)::BIGINT FROM gif_renditions")
            .fetch_one(&mut *tx)
            .await?;
    if (global_total + incoming) as u64 > global_max {
        return Ok(UploadOutcome::QuotaExceeded);
    }

    let (user_total,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(r.size_bytes),0)::BIGINT
         FROM gif_renditions r JOIN gifs g ON g.id = r.gif_id
         WHERE g.uploader_id = $1"
    )
    .bind(uploader_id)
    .fetch_one(&mut *tx)
    .await?;
    if (user_total + incoming) as u64 > per_user_max {
        return Ok(UploadOutcome::QuotaExceeded);
    }

    let gif = sqlx::query_as::<_, Gif>(&format!(
        "INSERT INTO gifs (id, filename, hash, uploader_id, visibility, is_nsfw, frame_count, duration_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING {GIF_COLS}"
    ))
    .bind(id).bind(filename).bind(hash).bind(uploader_id).bind(visibility).bind(is_nsfw)
    .bind(frame_count).bind(duration_ms)
    .fetch_one(&mut *tx)
    .await?;

    for r in renditions {
        sqlx::query(
            "INSERT INTO gif_renditions (gif_id, rendition, filename, width, height, size_bytes)
             VALUES ($1, $2, $3, $4, $5, $6)"
        )
        .bind(id).bind(r.rendition).bind(&r.filename).bind(r.width).bind(r.height).bind(r.size_bytes)
        .execute(&mut *tx)
        .await?;
    }

    for t in tags {
        sqlx::query("INSERT INTO tags (gif_id, tag) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(id).bind(t)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(UploadOutcome::Inserted(gif))
}

/// Replace an existing GIF's content in place: recompute its hash and metadata
/// and overwrite its three rendition rows (keeping their filenames, so the
/// on-disk files are overwritten, not relocated). The cap check accounts for
/// the *delta* between the old and new rendition sizes, under the same advisory
/// lock uploads use, so a concurrent upload/replace can't race past the cap.
/// Returns `Duplicate` if the new content matches another GIF the uploader
/// already owns (would violate `UNIQUE (uploader_id, hash)`), `QuotaExceeded`
/// if a cap would be exceeded, or `Updated` with the refreshed row.
#[allow(clippy::too_many_arguments)]
pub async fn replace_gif_quota(
    pool: &PgPool,
    id: &str,
    uploader_id: &str,
    filename: &str,
    hash: &str,
    frame_count: i32,
    duration_ms: i32,
    renditions: &[RenditionSpec],
    global_max: u64,
    per_user_max: u64,
) -> Result<ReplaceOutcome, AppError> {
    let incoming: i64 = renditions.iter().map(|r| r.size_bytes as i64).sum();
    let mut tx = pool.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(UPLOAD_LOCK_ID)
        .execute(&mut *tx)
        .await?;

    if sqlx::query_as::<_, Gif>(
        &format!("SELECT {GIF_COLS} FROM gifs WHERE uploader_id = $1 AND hash = $2 AND id <> $3")
    )
    .bind(uploader_id).bind(hash).bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .is_some() {
        return Ok(ReplaceOutcome::Duplicate);
    }

    // Bytes this GIF currently occupies; the replacement frees them and adds
    // `incoming`, so the cap check is against the net change.
    let (old_total,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(size_bytes),0)::BIGINT FROM gif_renditions WHERE gif_id = $1"
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    let (global_total,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(size_bytes),0)::BIGINT FROM gif_renditions")
            .fetch_one(&mut *tx)
            .await?;
    if (global_total - old_total + incoming) as u64 > global_max {
        return Ok(ReplaceOutcome::QuotaExceeded);
    }

    let (user_total,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(r.size_bytes),0)::BIGINT
         FROM gif_renditions r JOIN gifs g ON g.id = r.gif_id
         WHERE g.uploader_id = $1"
    )
    .bind(uploader_id)
    .fetch_one(&mut *tx)
    .await?;
    if (user_total - old_total + incoming) as u64 > per_user_max {
        return Ok(ReplaceOutcome::QuotaExceeded);
    }

    let gif = sqlx::query_as::<_, Gif>(&format!(
        "UPDATE gifs SET filename = $1, hash = $2, frame_count = $3, duration_ms = $4
         WHERE id = $5 RETURNING {GIF_COLS}"
    ))
    .bind(filename).bind(hash).bind(frame_count).bind(duration_ms).bind(id)
    .fetch_one(&mut *tx)
    .await?;

    for r in renditions {
        sqlx::query(
            "UPDATE gif_renditions SET width = $1, height = $2, size_bytes = $3
             WHERE gif_id = $4 AND rendition = $5"
        )
        .bind(r.width).bind(r.height).bind(r.size_bytes).bind(id).bind(r.rendition)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(ReplaceOutcome::Updated(gif))
}

pub async fn set_visibility(pool: &PgPool, id: &str, visibility: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE gifs SET visibility = $1 WHERE id = $2")
        .bind(visibility).bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_nsfw(pool: &PgPool, id: &str, is_nsfw: bool) -> Result<(), AppError> {
    sqlx::query("UPDATE gifs SET is_nsfw = $1 WHERE id = $2")
        .bind(is_nsfw).bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_gif(pool: &PgPool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM gifs WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_renditions(pool: &PgPool, gif_id: &str) -> Result<Vec<GifRendition>, AppError> {
    Ok(sqlx::query_as::<_, GifRendition>(
        "SELECT gif_id, rendition, filename, width, height, size_bytes FROM gif_renditions WHERE gif_id = $1"
    )
    .bind(gif_id)
    .fetch_all(pool)
    .await?)
}

/// Total bytes occupied by all stored renditions.
pub async fn total_storage_bytes(pool: &PgPool) -> Result<i64, AppError> {
    let (total,): (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM gif_renditions")
            .fetch_one(pool)
            .await?;
    Ok(total)
}

pub async fn get_tags(pool: &PgPool, gif_id: &str) -> Result<Vec<String>, AppError> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT tag FROM tags WHERE gif_id = $1 ORDER BY tag")
        .bind(gif_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(t,)| t).collect())
}

pub async fn set_tags(pool: &PgPool, gif_id: &str, tags: &[String]) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM tags WHERE gif_id = $1")
        .bind(gif_id)
        .execute(&mut *tx)
        .await?;
    for tag in tags {
        sqlx::query("INSERT INTO tags (gif_id, tag) VALUES ($1, $2)")
            .bind(gif_id).bind(tag)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn add_tags(pool: &PgPool, gif_id: &str, tags: &[String]) -> Result<(), AppError> {
    for tag in tags {
        sqlx::query("INSERT INTO tags (gif_id, tag) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(gif_id).bind(tag)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn remove_tags(pool: &PgPool, gif_id: &str, tags: &[String]) -> Result<(), AppError> {
    for tag in tags {
        sqlx::query("DELETE FROM tags WHERE gif_id = $1 AND tag = $2")
            .bind(gif_id).bind(tag)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn increment_uses(pool: &PgPool, gif_id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE gifs SET uses = uses + 1 WHERE id = $1")
        .bind(gif_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn search_by_tags(
    pool: &PgPool,
    terms: &[String],
    viewer: &str,
    mine: bool,
    grab_nsfw: bool,
    grab_hidden: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let patterns: Vec<String> = terms
        .iter()
        .map(|t| format!("{}%", t.replace('%', "").replace('_', "")))
        .collect();
    let vis = list_filters(mine, grab_nsfw, "g", "$4");
    let hidden = hidden_clause(grab_hidden, "g", "$4");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        r#"SELECT g.id, g.filename, g.hash, g.uploader_id, g.visibility, g.is_nsfw,
                  g.frame_count, g.duration_ms, g.uses, g.uploaded_at
           FROM gifs g
           JOIN tags t ON t.gif_id = g.id
           WHERE t.tag LIKE ANY($1) AND {vis}{hidden}
           GROUP BY g.id
           ORDER BY COUNT(t.tag) DESC, g.uses DESC, g.uploaded_at DESC
           LIMIT $2 OFFSET $3"#
    ))
    .bind(&patterns).bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

pub async fn search_by_filename(
    pool: &PgPool,
    query: &str,
    viewer: &str,
    mine: bool,
    grab_nsfw: bool,
    grab_hidden: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let vis = list_filters(mine, grab_nsfw, "gifs", "$4");
    let hidden = hidden_clause(grab_hidden, "gifs", "$4");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        r#"SELECT {GIF_COLS}
           FROM gifs
           WHERE to_tsvector('english', filename) @@ plainto_tsquery('english', $1)
             AND {vis}{hidden}
           ORDER BY ts_rank(to_tsvector('english', filename), plainto_tsquery('english', $1)) DESC,
                    uses DESC
           LIMIT $2 OFFSET $3"#
    ))
    .bind(query).bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

pub async fn list_featured(
    pool: &PgPool,
    viewer: &str,
    mine: bool,
    grab_nsfw: bool,
    grab_hidden: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let vis = list_filters(mine, grab_nsfw, "gifs", "$3");
    let hidden = hidden_clause(grab_hidden, "gifs", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT {GIF_COLS} FROM gifs WHERE {vis}{hidden}
         ORDER BY uses DESC, uploaded_at DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

/// Like `list_featured` but ordered by a seeded deterministic hash of each
/// GIF's id, so a given `seed` produces the same stable shuffle across paged
/// requests (unlike `ORDER BY RANDOM()`, which would re-shuffle per call).
/// Visibility/NSFW/hidden filtering is identical to `list_featured`.
pub async fn list_featured_random(
    pool: &PgPool,
    viewer: &str,
    mine: bool,
    grab_nsfw: bool,
    grab_hidden: bool,
    limit: i64,
    offset: i64,
    seed: i64,
) -> Result<Vec<Gif>, AppError> {
    let vis = list_filters(mine, grab_nsfw, "gifs", "$3");
    let hidden = hidden_clause(grab_hidden, "gifs", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT {GIF_COLS} FROM gifs WHERE {vis}{hidden}
         ORDER BY md5($4::text || id) LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer).bind(seed)
    .fetch_all(pool)
    .await?)
}

pub async fn list_recent(
    pool: &PgPool,
    viewer: &str,
    mine: bool,
    grab_nsfw: bool,
    grab_hidden: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let vis = list_filters(mine, grab_nsfw, "gifs", "$3");
    let hidden = hidden_clause(grab_hidden, "gifs", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT {GIF_COLS} FROM gifs WHERE {vis}{hidden}
         ORDER BY uploaded_at DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

/// Tag prefix autocomplete. Only considers tags appearing on at least one
/// shared GIF that is not NSFW (unless `grab_nsfw`). Tags that exist solely
/// on private GIFs (anyone's, including the requester's) or NSFW GIFs are
/// never suggested.
pub async fn suggest_tags(
    pool: &PgPool,
    prefix: &str,
    grab_nsfw: bool,
    limit: i64,
) -> Result<Vec<String>, AppError> {
    let safe = prefix.replace('%', "").replace('_', "");
    let pattern = format!("{}%", safe);
    let nsfw_filter = if grab_nsfw { "" } else { " AND NOT g.is_nsfw" };
    let rows: Vec<(String,)> = sqlx::query_as(&format!(
        "SELECT t.tag
         FROM tags t
         JOIN gifs g ON g.id = t.gif_id
         WHERE t.tag LIKE $1 AND g.visibility = 'shared'{nsfw_filter}
         GROUP BY t.tag
         ORDER BY COUNT(*) DESC
         LIMIT $2"
    ))
    .bind(&pattern).bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(t,)| t).collect())
}

pub async fn add_favorite(pool: &PgPool, mxid: &str, gif_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO favorites (mxid, gif_id) VALUES ($1, $2) ON CONFLICT DO NOTHING"
    )
    .bind(mxid).bind(gif_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_favorite(pool: &PgPool, mxid: &str, gif_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM favorites WHERE mxid = $1 AND gif_id = $2")
        .bind(mxid).bind(gif_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn add_hidden(pool: &PgPool, mxid: &str, gif_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO hidden (mxid, gif_id) VALUES ($1, $2) ON CONFLICT DO NOTHING"
    )
    .bind(mxid).bind(gif_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_hidden(pool: &PgPool, mxid: &str, gif_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM hidden WHERE mxid = $1 AND gif_id = $2")
        .bind(mxid).bind(gif_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record that `mxid` selected `gif_id` (updates their history timestamp) and
/// return whether this selection should count toward the GIF's `uses` ranking.
/// A user's selection of a given GIF counts at most once per 24h, so the
/// ranking can't be inflated by repeated selects.
pub async fn record_selection(
    pool: &PgPool,
    mxid: &str,
    gif_id: &str,
) -> Result<bool, AppError> {
    // First-ever selection of this GIF by this user: row is created and the
    // selection counts. ON CONFLICT DO NOTHING makes concurrent first selects
    // resolve to exactly one creator.
    let created = sqlx::query(
        "INSERT INTO selections (mxid, gif_id, used_at) VALUES ($1, $2, now())
         ON CONFLICT (mxid, gif_id) DO NOTHING"
    )
    .bind(mxid).bind(gif_id)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    if created {
        return Ok(true);
    }

    // Row exists. Lock it, decide based on the *prior* timestamp whether 24h
    // have elapsed, then refresh it — all in one atomic statement so
    // concurrent selects can't both count.
    let counts: bool = sqlx::query_scalar(
        "WITH cur AS (
             SELECT used_at <= now() - interval '1 day' AS stale
             FROM selections
             WHERE mxid = $1 AND gif_id = $2
             FOR UPDATE
         ), upd AS (
             UPDATE selections SET used_at = now()
             WHERE mxid = $1 AND gif_id = $2
         )
         SELECT stale FROM cur"
    )
    .bind(mxid).bind(gif_id)
    .fetch_one(pool)
    .await?;
    Ok(counts)
}

pub async fn list_favorites(
    pool: &PgPool,
    viewer: &str,
    grab_nsfw: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let filters = list_filters(false, grab_nsfw, "g", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT g.id, g.filename, g.hash, g.uploader_id, g.visibility, g.is_nsfw,
                g.frame_count, g.duration_ms, g.uses, g.uploaded_at
         FROM gifs g
         JOIN favorites f ON f.gif_id = g.id
         WHERE f.mxid = $3 AND {filters}
         ORDER BY f.created_at DESC
         LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

/// The viewer's hidden GIFs, newest first. This is the management view used
/// to un-hide, so it ignores the hidden filter by design; it still respects
/// visibility (a GIF turned private by its uploader stays unreachable).
pub async fn list_hidden(
    pool: &PgPool,
    viewer: &str,
    grab_nsfw: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let filters = list_filters(false, grab_nsfw, "g", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT g.id, g.filename, g.hash, g.uploader_id, g.visibility, g.is_nsfw,
                g.frame_count, g.duration_ms, g.uses, g.uploaded_at
         FROM gifs g
         JOIN hidden hd ON hd.gif_id = g.id
         WHERE hd.mxid = $3 AND {filters}
         ORDER BY hd.created_at DESC
         LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

pub async fn list_history(
    pool: &PgPool,
    viewer: &str,
    grab_nsfw: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Gif>, AppError> {
    let filters = list_filters(false, grab_nsfw, "g", "$3");
    Ok(sqlx::query_as::<_, Gif>(&format!(
        "SELECT g.id, g.filename, g.hash, g.uploader_id, g.visibility, g.is_nsfw,
                g.frame_count, g.duration_ms, g.uses, g.uploaded_at
         FROM gifs g
         JOIN selections s ON s.gif_id = g.id
         WHERE s.mxid = $3 AND {filters}
         ORDER BY s.used_at DESC
         LIMIT $1 OFFSET $2"
    ))
    .bind(limit).bind(offset).bind(viewer)
    .fetch_all(pool)
    .await?)
}

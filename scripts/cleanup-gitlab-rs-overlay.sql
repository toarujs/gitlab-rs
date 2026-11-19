-- gitlab-rs overlay tables. Run before switching compose image back to official GitLab CE.
-- Invite tokens currently live in Redis (gitlab-rs:*). Add DROP statements here when
-- batch invite / custom expiry introduce SQL tables.

-- DROP TABLE IF EXISTS gitlab_rs_invite_links;
SELECT 'gitlab-rs overlay: no extra SQL tables yet' AS status;

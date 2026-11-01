CREATE TABLE page_geometry (
  page_id TEXT PRIMARY KEY NOT NULL REFERENCES page(id) ON DELETE CASCADE,
  geometry_version TEXT NOT NULL,
  geometry_json TEXT NOT NULL
);

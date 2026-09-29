import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { fromDirectoryHandle, fromFileList } from "./local-bundle.mjs";

function directoryHandle(path) {
  const fileHandle = (name) => ({
    kind: "file", name,
    getFile: async () => ({ text: () => readFile(join(path, name), "utf8") }),
  });
  return {
    kind: "directory", name: basename(path),
    async getDirectoryHandle(name) {
      await readdir(join(path, name));
      return directoryHandle(join(path, name));
    },
    async getFileHandle(name) {
      await readFile(join(path, name));
      return fileHandle(name);
    },
    async *entries() {
      for (const entry of await readdir(path, { withFileTypes: true })) {
        yield [entry.name, entry.isDirectory()
          ? directoryHandle(join(path, entry.name)) : fileHandle(entry.name)];
      }
    },
  };
}

test("all source loaders retain SQL dumps and Rails schema configuration", async () => {
  const temp = await mkdtemp(join(tmpdir(), "roundhouse-bundle-"));
  try {
    const root = join(temp, "postgres-blog");
    await cp(new URL("../../fixtures/postgres-blog", import.meta.url), root, { recursive: true });
    const extraFiles = {
      "db/schema.rb": "ActiveRecord::Schema.define do; create_table :stale; end",
      "config/environments/production.rb": "Rails.application.configure do; config.active_record.schema_format = :sql; end",
      "config/initializers/schema.rb": "Rails.application.config.active_record.schema_format = :sql",
      "config/routes.rb": "Rails.application.routes.draw do; end",
      "config/routes/admin.rb": "resources :articles",
      "config/database.yml": "excluded: true",
      "db/unrelated.sql": "SELECT 1;",
      "public/app.js": "excluded",
    };
    for (const [path, text] of Object.entries(extraFiles)) {
      await mkdir(dirname(join(root, path)), { recursive: true });
      await writeFile(join(root, path), text);
    }
    const files = [];
    async function walk(path = "") {
      for (const entry of await readdir(join(root, path), { withFileTypes: true })) {
        const rel = path ? `${path}/${entry.name}` : entry.name;
        if (entry.isDirectory()) await walk(rel);
        else files.push({
          webkitRelativePath: `postgres-blog/${rel}`,
          text: () => readFile(join(root, rel), "utf8"),
        });
      }
    }
    await walk();
    const viaFiles = await fromFileList(files);
    const viaHandle = await fromDirectoryHandle(directoryHandle(root));
    const output = join(temp, "bundle.json");
    execFileSync(process.execPath, [
      fileURLToPath(new URL("../ide/bundle-src.mjs", import.meta.url)), root, output,
    ], { stdio: "pipe" });
    const viaNode = JSON.parse(await readFile(output, "utf8"));
    const expectedPaths = [
      "app/models/article.rb", "app/models/author.rb",
      "db/schema.rb", "db/structure.sql", "config/application.rb",
      "config/environments/production.rb", "config/initializers/schema.rb",
      "config/routes.rb", "config/routes/admin.rb",
    ].sort();
    assert.deepEqual(Object.keys(viaNode.src).sort(), expectedPaths);
    assert.deepEqual(viaHandle.src, viaNode.src);
    assert.deepEqual(viaFiles.src, viaNode.src);
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
});

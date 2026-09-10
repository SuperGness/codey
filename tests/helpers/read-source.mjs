import { readFile } from "node:fs/promises";

const root = new URL("../../", import.meta.url);

export const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

// Reads a repository file relative to the repo root with CRLF normalized so
// source-contract assertions behave the same on Windows checkouts.
export const readSource = async (path) =>
  normalizeLineEndings(await readFile(new URL(path, root), "utf8"));

// Counter module
import { readFile } from "node:fs/promises";

const LIMIT = 100;

export class Counter {
  constructor(start) {
    this.value = start ?? 0;
  }

  increment(by = 1) {
    this.value += by;
    return this;
  }
}

async function load(path) {
  const text = await readFile(path, "utf8");
  const re = /ab+c/gi;
  return text.split("\n").filter((line) => re.test(line)).length > LIMIT;
}

const obj = { name: "x", run() { return true; }, n: 0.5, flag: null };

/**
 * Record live Ergo node responses into tests/fixtures/.
 *
 * Usage:
 *   node scripts/record-fixtures.ts [--node URL] [--out DIR]
 *       [--address ADDR] [--small-token TOKEN_ID]
 *
 * Runs on Node 23.6+ (native type stripping) with no dependencies.
 * The node URL defaults to $ERGO_NODE_URL. The node must run the extra indexer.
 * Samples (an address with several boxes, a token with emission <= 100 spread
 * over several boxes) are discovered from recent blocks unless given explicitly.
 *
 * Every fixture is the response body, pretty-printed with numbers kept exactly as
 * the node sent them (some exceed 2^53). manifest.json records for each one the
 * request, the HTTP status, and notes. Fixtures marked "synthetic" are derived from
 * a real response and say how in their notes.
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const UNKNOWN_ID = "ab".repeat(32);
// Valid mainnet P2PK address (Fleet SDK test vectors) that holds no boxes.
const EMPTY_ADDRESS = "9gNvAv97W71Wm33GoXgSQBFJxinFubKvE6wh2dEhFTSgYEe783j";
const SMALL_SUPPLY_LIMIT = 100;
const BLOCKS_TO_SCAN = 50;
const MEMPOOL_LIMIT = 20;

type Json = any;
type Method = "GET" | "POST";

interface Response {
  status: number;
  /** Parsed with plain JSON.parse: fine for logic, lossy above 2^53. */
  value: Json;
  /** Parsed keeping every number's source text, for writing fixtures. */
  exact: Json;
  /** The raw body, for taking an independent exact copy with parseExact(). */
  text: string;
}

interface ManifestEntry {
  method: Method;
  path: string;
  status: number;
  body?: Json;
  synthetic?: true;
  notes?: string;
}

/** JSON.parse that keeps each number's exact source text (JSON.rawJSON, Node 21+). */
function parseExact(text: string): Json {
  return JSON.parse(text, (_key, value, context) =>
    typeof value === "number" ? (JSON as any).rawJSON(context.source) : value,
  );
}

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

class Node {
  readonly baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
  }

  async request(method: Method, path: string, body?: Json): Promise<Response> {
    const res = await fetch(this.baseUrl + path, {
      method,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(60_000),
    });
    const text = await res.text();
    return {
      status: res.status,
      value: text ? JSON.parse(text) : null,
      exact: text ? parseExact(text) : null,
      text,
    };
  }

  async ok(method: Method, path: string, body?: Json): Promise<Json> {
    const res = await this.request(method, path, body);
    if (res.status !== 200) {
      fail(`${method} ${path} returned ${res.status}: ${JSON.stringify(res.value)}`);
    }
    return res.value;
  }
}

class Recorder {
  readonly node: Node;
  readonly outDir: string;
  readonly manifest: Record<string, ManifestEntry> = {};

  constructor(node: Node, outDir: string) {
    this.node = node;
    this.outDir = outDir;
  }

  async record(name: string, method: Method, path: string, body?: Json, notes = "") {
    const res = await this.node.request(method, path, body);
    this.save(name, { method, path, status: res.status, body, notes }, res.exact);
    return res;
  }

  save(name: string, entry: ManifestEntry, payload: Json) {
    writeFileSync(join(this.outDir, `${name}.json`), JSON.stringify(payload, null, 2) + "\n");
    const clean: ManifestEntry = { method: entry.method, path: entry.path, status: entry.status };
    if (entry.body !== undefined) clean.body = entry.body;
    if (entry.synthetic) clean.synthetic = true;
    if (entry.notes) clean.notes = entry.notes;
    this.manifest[name] = clean;
    console.log(`  ${name}.json  [${entry.status}]`);
  }
}

interface Samples {
  address: string;
  nft: string;
  small: string;
  large: string;
}

/** Pick sample ids from recent indexed blocks (newest first). */
async function discover(
  node: Node,
  blocks: AsyncIterable<Json>,
  given: { address?: string; smallToken?: string },
): Promise<Samples> {
  const emissions = new Map<string, number | null>();
  const emission = async (tokenId: string) => {
    if (!emissions.has(tokenId)) {
      const res = await node.request("GET", `/blockchain/token/byId/${tokenId}`);
      emissions.set(tokenId, res.status === 200 ? res.value.emissionAmount : null);
    }
    return emissions.get(tokenId)!;
  };

  let address = given.address;
  let small = given.smallToken;
  let nft: string | undefined;
  let large: string | undefined;

  for await (const block of blocks) {
    for (const tx of block.transactions) {
      const outputs: Json[] = tx.outputs;

      if (address === undefined) {
        for (const box of outputs) {
          if (!box.ergoTree.startsWith("0008cd")) continue;
          const found = await node.ok(
            "POST",
            "/blockchain/box/unspent/byAddress?limit=5",
            box.address,
          );
          if (found.length >= 2) {
            address = box.address;
            break;
          }
        }
      }

      const tokenIds = new Set<string>(outputs.flatMap((b) => b.assets.map((a: Json) => a.tokenId)));
      for (const tokenId of tokenIds) {
        const amount = await emission(tokenId);
        if (amount === null) continue;
        if (amount === 1) {
          nft ??= tokenId;
        } else if (amount > SMALL_SUPPLY_LIMIT) {
          large ??= tokenId;
        } else if (amount >= 2 && small === undefined) {
          const boxes = await node.ok(
            "GET",
            `/blockchain/box/unspent/byTokenId/${tokenId}?limit=100`,
          );
          if (boxes.length >= 2) small = tokenId;
        }
      }
    }
    if (address && nft && large && small) break;
  }

  const missing = Object.entries({
    address,
    nft,
    "large token": large,
    "small-supply token": small,
  })
    .filter(([, v]) => v === undefined)
    .map(([k]) => k);
  if (missing.length) fail(`Could not discover: ${missing.join(", ")}. Pass them explicitly.`);

  return { address: address!, nft: nft!, small: small!, large: large! };
}

async function main() {
  const { values: args } = parseArgs({
    options: {
      node: { type: "string", default: process.env.ERGO_NODE_URL },
      out: { type: "string", default: fileURLToPath(new URL("../tests/fixtures", import.meta.url)) },
      address: { type: "string" },
      "small-token": { type: "string" },
    },
  });
  if (!args.node) fail("Pass --node or set ERGO_NODE_URL");

  const node = new Node(args.node);
  const outDir = resolve(args.out!);
  mkdirSync(outDir, { recursive: true });
  const rec = new Recorder(node, outDir);

  const info = await node.ok("GET", "/info");
  const indexed = await node.ok("GET", "/blockchain/indexedHeight");
  if (indexed.indexedHeight !== indexed.fullHeight) {
    fail(`Indexer is behind: ${JSON.stringify(indexed)}`);
  }

  console.log("Scanning recent blocks for samples...");
  const headers: Json[] = await node.ok("GET", `/blocks/lastHeaders/${BLOCKS_TO_SCAN}`);
  // Newest first, skipping the tip so the sample block is less likely to be reorged.
  headers.sort((a, b) => b.height - a.height);
  headers.shift();

  let sampleBlock: Json;
  async function* blocks() {
    for (const header of headers) {
      const block = await node.ok("GET", `/blockchain/block/byHeaderId/${header.id}`);
      sampleBlock ??= block;
      yield block;
    }
  }

  const { address, nft, small, large } = await discover(node, blocks(), {
    address: args.address,
    smallToken: args["small-token"],
  });
  console.log(
    `  address: ${address}\n  nft: ${nft}\n  small-supply token: ${small}\n  large token: ${large}`,
  );
  console.log(`Recording into ${outDir}`);

  // Chain and node state
  await rec.record("info", "GET", "/info");
  await rec.record("indexed_height", "GET", "/blockchain/indexedHeight");
  await rec.record("last_headers", "GET", "/blocks/lastHeaders/3");

  // Blocks
  const headerId: string = sampleBlock.header.id;
  await rec.record(
    "indexed_block",
    "GET",
    `/blockchain/block/byHeaderId/${headerId}`,
    undefined,
    "Inputs are full indexed boxes. The live shape differs from the spec " +
      "(top-level header/transactions/height/size).",
  );
  await rec.record("indexed_block_unknown", "GET", `/blockchain/block/byHeaderId/${UNKNOWN_ID}`);
  await rec.record(
    "block",
    "GET",
    `/blocks/${headerId}`,
    undefined,
    "Plain block: inputs carry only boxId and spendingProof.",
  );

  // Mempool
  const mempool = await rec.record("mempool", "GET", `/transactions/unconfirmed?limit=${MEMPOOL_LIMIT}`);
  const unresolved = mempool.value.some((tx: Json) => tx.inputs.some((i: Json) => !("value" in i)));
  if (mempool.exact.length && !unresolved) {
    const txs = parseExact(mempool.text);
    const first = txs[0].inputs[0];
    txs[0].inputs[0] = { boxId: first.boxId, spendingProof: first.spendingProof };
    rec.save(
      "mempool_unresolved_input",
      {
        method: "GET",
        path: "/transactions/unconfirmed",
        status: 200,
        synthetic: true,
        notes:
          "Copy of mempool.json with the first tx's first input reduced to " +
          "boxId + spendingProof, as the node returns for ergo#2248. " +
          "No such input was live when recording.",
      },
      txs,
    );
  } else if (unresolved) {
    console.log("  (live mempool already contains an unresolved input)");
  }

  // Address
  const unspentPath = "/blockchain/box/unspent/byAddress?limit=5&sortDirection=asc";
  await rec.record("unspent_by_address", "POST", unspentPath, address);
  await rec.record(
    "unspent_by_address_empty",
    "POST",
    unspentPath,
    EMPTY_ADDRESS,
    "Empty set is 200 [] (the spec says 404).",
  );
  await rec.record("balance", "POST", "/blockchain/balance", address);
  await rec.record("balance_empty", "POST", "/blockchain/balance", EMPTY_ADDRESS);

  // Tokens
  const unspentByToken = (id: string) =>
    `/blockchain/box/unspent/byTokenId/${id}?limit=100&sortDirection=asc`;
  await rec.record("token_nft", "GET", `/blockchain/token/byId/${nft}`);
  await rec.record("token_small_supply", "GET", `/blockchain/token/byId/${small}`);
  await rec.record("token_large_supply", "GET", `/blockchain/token/byId/${large}`);
  await rec.record("token_unknown", "GET", `/blockchain/token/byId/${UNKNOWN_ID}`);
  await rec.record("unspent_by_token_nft", "GET", unspentByToken(nft));
  await rec.record("unspent_by_token_small_supply", "GET", unspentByToken(small));
  await rec.record(
    "unspent_by_token_unknown",
    "GET",
    unspentByToken(UNKNOWN_ID),
    undefined,
    "Unknown token is 200 [] (the spec says 404).",
  );

  // Boxes
  const spentId: string = sampleBlock.transactions[0].inputs[0].boxId;
  await rec.record(
    "box_by_id_spent",
    "GET",
    `/blockchain/box/byId/${spentId}`,
    undefined,
    "Spent box: spentTransactionId is set, spendingHeight is absent.",
  );
  await rec.record("box_by_id_unknown", "GET", `/blockchain/box/byId/${UNKNOWN_ID}`);
  const [unspent] = await node.ok("POST", "/blockchain/box/unspent/byAddress?limit=1", address);
  await rec.record(
    "with_pool_partial",
    "POST",
    "/utxo/withPool/byIds",
    [UNKNOWN_ID, unspent.boxId],
    "Missing ids are silently omitted.",
  );
  await rec.record(
    "with_pool_none",
    "POST",
    "/utxo/withPool/byIds",
    [UNKNOWN_ID],
    "No id exists: 200 [] (the spec says 404).",
  );

  const manifest = {
    recordedAt: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
    nodeVersion: info.appVersion,
    network: info.network,
    fullHeight: info.fullHeight,
    samples: { address, nft, smallSupplyToken: small, largeSupplyToken: large, headerId },
    fixtures: rec.manifest,
  };
  writeFileSync(join(outDir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`Wrote ${Object.keys(rec.manifest).length} fixtures and manifest.json`);
}

await main();

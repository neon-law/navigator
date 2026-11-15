const assert = require("node:assert/strict");
const test = require("node:test");
const Graph = require("../public/vendor/graphology/graphology.umd.min.js");
const { buildGraph, layout } = require("../public/js/graph-view.js");

test("Project, lawyer and Firm graphs render observations without projected nodes", () => {
  for (const source_kind of ["Project", "Person", "Firm"]) {
    const observation = {
      id: "log-1",
      source: "seed",
      source_name: "Synthetic seed",
      source_kind,
      subject_id: "witness",
      subject_name: "Fixture Witness",
      subject_kind: "Person",
      action: "witness",
    };
    const payload = { nodes: [], edges: [], relationship_logs: [observation] };
    const graph = layout(buildGraph(Graph, payload), 2);

    assert.equal(graph.order, 2);
    assert.equal(graph.size, 1);
    assert.equal(graph.getNodeAttribute("witness", "label"), "Fixture Witness");
    assert.equal(graph.getEdgeAttribute(graph.edges()[0], "tooltip"), "witness · Relationship Log");
    assert.deepEqual(payload.nodes, []);
    assert.deepEqual(payload.edges, []);
  }
});

test("distinct observations survive when an existing edge joins the same nodes", () => {
  const entry = {
    id: "log-1", action: "witness", source: "seed", source_name: "Lawyer",
    source_kind: "Person", subject_id: "contact", subject_name: "Fixture Witness",
    subject_kind: "Person",
  };
  const graph = buildGraph(Graph, {
    edges: [{ source: "seed", target: "contact", type: "RELATIONSHIP", kind: "related_party" }],
    relationship_logs: [entry, { ...entry, id: "log-2", action: "co_counsel" }],
  });
  assert.equal(graph.size, 3);
  assert.equal(graph.getEdgeAttribute("relationship-log:log-1", "label"), "witness · Relationship Log");
  assert.equal(graph.getEdgeAttribute("relationship-log:log-2", "label"), "co_counsel · Relationship Log");
});

test("the browser finishes mounting a graph that contains an observation", {
  skip: !process.env.CHROME_BINARY,
}, async (context) => {
  const { execFile } = require("node:child_process");
  const fs = require("node:fs/promises");
  const http = require("node:http");
  const os = require("node:os");
  const path = require("node:path");
  const { promisify } = require("node:util");
  const profile = await fs.mkdtemp(path.join(os.tmpdir(), "navigator-graph-browser-"));
  context.after(() => fs.rm(profile, { recursive: true, force: true }));
  const payload = {
    nodes: [], edges: [], relationship_logs: [{
      id: "log-1", source: "seed", source_name: "Synthetic project", source_kind: "Project",
      subject_id: "witness", subject_name: "Fixture Witness", subject_kind: "Person",
      action: "witness",
    }],
  };
  const scripts = new Set([
    "/public/js/graph-view.js",
    "/public/vendor/graphology/graphology.umd.min.js",
    "/public/vendor/sigma/sigma.min.js",
  ]);
  const server = http.createServer(async (request, response) => {
    if (request.url === "/") {
      response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
      response.end('<!doctype html><div class="graph-view" data-endpoint="/fixture" ' +
        'style="position:relative;width:420px;height:240px">' +
        '<p class="graph-view__status">Loading graph...</p></div>' +
        '<script src="/public/js/graph-view.js"></script>');
    } else if (request.url === "/fixture") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify(payload));
    } else if (scripts.has(request.url)) {
      response.writeHead(200, { "Content-Type": "text/javascript; charset=utf-8" });
      response.end(await fs.readFile(path.join(__dirname, "..", request.url)));
    } else {
      response.writeHead(404).end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  context.after(() => server.close());
  const { stdout } = await promisify(execFile)(process.env.CHROME_BINARY, [
    "--headless", "--no-sandbox", "--no-first-run", "--use-gl=angle",
    "--use-angle=swiftshader", "--enable-unsafe-swiftshader",
    `--user-data-dir=${profile}`, "--virtual-time-budget=6000", "--dump-dom",
    `http://127.0.0.1:${server.address().port}/`,
  ], { timeout: 30000 });

  assert.match(stdout, /<canvas class="sigma-nodes"/);
  // Sigma creates canvases before constructing the edge programs. An
  // exception in the constructor leaves the loading status visible.
  assert.match(stdout, /class="graph-view__status" hidden=""/);
  assert.match(stdout, /class="graph-tooltip"/);
  assert.match(stdout, /aria-description="Relationship Log · witness · Fixture Witness"/);
});

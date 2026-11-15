const assert = require("node:assert/strict");
const test = require("node:test");
const Graph = require("../public/vendor/graphology/graphology.umd.min.js");
const { buildGraph, layout } = require("../public/js/graph-view.js");

test("Project, lawyer and Firm graphs render observations without projected nodes", () => {
  for (const source_kind of ["Project", "Person", "Firm"]) {
    const observation = {
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

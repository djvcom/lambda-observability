window.BENCHMARK_DATA = {
  "lastUpdate": 1787920054380,
  "repoUrl": "https://github.com/djvcom/lambda-observability",
  "entries": {
    "opentelemetry-lambda-extension": [
      {
        "commit": {
          "author": {
            "email": "41898282+github-actions[bot]@users.noreply.github.com",
            "name": "github-actions[bot]",
            "username": "github-actions[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c75c6f8a5fdd18540311868ba0861456648fda98",
          "message": "chore: release (#133)\n\nCo-authored-by: github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com>",
          "timestamp": "2026-08-28T13:10:28+01:00",
          "tree_id": "aa73a4eec51e35ef2b5156afc0896de8a88d0bcd",
          "url": "https://github.com/djvcom/lambda-observability/commit/c75c6f8a5fdd18540311868ba0861456648fda98"
        },
        "date": 1787919911384,
        "tool": "cargo",
        "benches": [
          {
            "name": "aggregator_add/within_budget",
            "value": 88685,
            "range": "± 5804",
            "unit": "ns/iter"
          },
          {
            "name": "aggregator_add/evicting",
            "value": 93527,
            "range": "± 3640",
            "unit": "ns/iter"
          },
          {
            "name": "aggregator_drain/get_all_batches_1000_signals",
            "value": 45088,
            "range": "± 1075",
            "unit": "ns/iter"
          },
          {
            "name": "MetricsConverter::convert_report",
            "value": 472,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "SpanConverter::create_invocation_span",
            "value": 426,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "TelemetryProcessor::process_events",
            "value": 1189,
            "range": "± 18",
            "unit": "ns/iter"
          },
          {
            "name": "MetricsConverter::convert_report (with resource)",
            "value": 613,
            "range": "± 18",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/100KiB",
            "value": 123554,
            "range": "± 2948",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/1MiB",
            "value": 1347846,
            "range": "± 11701",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/4MiB",
            "value": 5412699,
            "range": "± 50270",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/100KiB",
            "value": 737438,
            "range": "± 1728",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/1MiB",
            "value": 7463188,
            "range": "± 29344",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/4MiB",
            "value": 28751577,
            "range": "± 139840",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "44838735+djvcom@users.noreply.github.com",
            "name": "Daniel Verrall",
            "username": "djvcom"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "9a02a01a71aa47237905d0dba2d91a67dac4f3a1",
          "message": "chore: republish the simulator and extension as 0.2.0 (#135)\n\nThe 0.1.8 simulator and 0.1.9 extension releases carried breaking\nchanges under patch versions: the simulator's error variants changed\npayload type, and the extension removed public types and changed\nseveral signatures. Both crates take the minor bump those changes\nrequired, and the mislabelled versions are yanked so a caret\nrequirement on 0.1 cannot pull them in.",
          "timestamp": "2026-08-28T13:22:03+01:00",
          "tree_id": "9d84781d7dc676e8a1cabb01f953449dbcc10270",
          "url": "https://github.com/djvcom/lambda-observability/commit/9a02a01a71aa47237905d0dba2d91a67dac4f3a1"
        },
        "date": 1787920052607,
        "tool": "cargo",
        "benches": [
          {
            "name": "aggregator_add/within_budget",
            "value": 74905,
            "range": "± 7733",
            "unit": "ns/iter"
          },
          {
            "name": "aggregator_add/evicting",
            "value": 76035,
            "range": "± 5710",
            "unit": "ns/iter"
          },
          {
            "name": "aggregator_drain/get_all_batches_1000_signals",
            "value": 42112,
            "range": "± 2045",
            "unit": "ns/iter"
          },
          {
            "name": "MetricsConverter::convert_report",
            "value": 372,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "SpanConverter::create_invocation_span",
            "value": 364,
            "range": "± 4",
            "unit": "ns/iter"
          },
          {
            "name": "TelemetryProcessor::process_events",
            "value": 920,
            "range": "± 20",
            "unit": "ns/iter"
          },
          {
            "name": "MetricsConverter::convert_report (with resource)",
            "value": 496,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/100KiB",
            "value": 74438,
            "range": "± 626",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/1MiB",
            "value": 860588,
            "range": "± 3784",
            "unit": "ns/iter"
          },
          {
            "name": "export_encode/protobuf/4MiB",
            "value": 3549001,
            "range": "± 22638",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/100KiB",
            "value": 548309,
            "range": "± 10460",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/1MiB",
            "value": 5539650,
            "range": "± 14258",
            "unit": "ns/iter"
          },
          {
            "name": "export_gzip/gzip/4MiB",
            "value": 20854369,
            "range": "± 58663",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}
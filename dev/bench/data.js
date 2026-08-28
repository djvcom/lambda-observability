window.BENCHMARK_DATA = {
  "lastUpdate": 1787919912306,
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
      }
    ]
  }
}
#!/usr/bin/env python3
"""Measure context edits on controlled pages derived from one real corpus match."""

import argparse
import hashlib
import json
import re
import shutil
import statistics
import subprocess
import time
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "url", help="Running question UI with a query that returns examples"
)
args = parser.parse_args()
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
driver = subprocess.Popen(
    ["chromedriver", "--port=0"],
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
)
session = None
try:
    for line in driver.stdout:
        if match := re.search(r"started successfully on port (\d+)", line):
            base = "http://127.0.0.1:" + match[1]
            break
    else:
        raise RuntimeError("ChromeDriver did not start")

    def request(path, body=None, method=None):
        with http.open(
            urllib.request.Request(
                base + path,
                data=None if body is None else json.dumps(body).encode(),
                method=method,
                headers={"Content-Type": "application/json"},
            ),
            timeout=120,
        ) as response:
            return json.load(response)["value"]

    started = request(
        "/session",
        {
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "chrome",
                    "goog:chromeOptions": {
                        "binary": shutil.which("chromium"),
                        "args": [
                            "--headless",
                            "--no-sandbox",
                            "--disable-dev-shm-usage",
                        ],
                    },
                }
            }
        },
    )
    session = started["sessionId"]
    prefix = "/session/" + session

    def javascript(source):
        return request(prefix + "/execute/sync", {"script": source, "args": []})

    def wait(expression):
        deadline = time.monotonic() + 90
        while not javascript("return " + expression):
            if time.monotonic() >= deadline:
                raise RuntimeError("Timed out waiting for " + expression)
            time.sleep(0.05)

    parsed = urllib.parse.urlsplit(args.url)
    settings = dict(urllib.parse.parse_qsl(parsed.fragment))
    settings.update(fresh="1", run="0")
    initial = urllib.parse.urlunsplit(
        parsed._replace(fragment=urllib.parse.urlencode(settings))
    )
    request(prefix + "/window/rect", {"width": 1440, "height": 1000})
    request(prefix + "/url", {"url": initial})
    wait("!!window.__probe")
    javascript("document.querySelector('#query-form').requestSubmit()")
    wait(
        "__probe.state.current?.phase==='complete' && document.querySelector('.kwic-row') && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'"
    )
    javascript("""return (async()=>{
      const run=__probe.state.current;
      window.__pageTemplate=await (await fetch('api/questions/runs/'+run.id+'/documents?'+new URLSearchParams({pattern:run.definitions[0],limit:'1',continuous:'1',after:'-1'}))).json();
      window.__realFetch=fetch;
      window.fetch=(url,...args)=>String(url).includes('/documents?')?Promise.resolve(new Response(JSON.stringify(__measuredPage),{headers:{'Content-Type':'application/json'}})):__realFetch(url,...args);
    })()""")
    results = []
    for size, per_document in [
        (500, 500),
        (2000, 2000),
        (8000, 8000),
        (500, 4),
        (2000, 4),
        (8000, 4),
    ]:
        javascript(
            """const size="""
            + str(size)
            + """;
          const first=__pageTemplate.rows[0],perDocument="""
            + str(per_document)
            + """;
          const documents=Array.from({length:Math.ceil(size/perDocument)},(_,ordinal)=>({...__pageTemplate.documents[0],ordinal,title:__pageTemplate.documents[0].title+' '+ordinal,matches:Math.min(perDocument,size-ordinal*perDocument)}));
          window.__measuredPage={...__pageTemplate,matches_total:size,documents_total:documents.length,documents:documents.slice(0,100),landmarks:documents,next:null,previous:null,rows:Array.from({length:size},(_,sequence)=>({...first,sequence,document_ordinal:Math.floor(sequence/perDocument),source_metadata:{...first.source_metadata,document:{...first.source_metadata?.document,title:documents[Math.floor(sequence/perDocument)].title}}}))};
          __probe.state.current.counts[__probe.state.current.definitions[0]]=size;
          document.querySelector('#context-size').dispatchEvent(new Event('input'));
          document.querySelector('#line-limit').value=size;
          document.querySelector('#line-limit').dispatchEvent(new Event('input'));
        """
        )
        wait(
            "document.querySelectorAll('.kwic-row').length==="
            + str(size)
            + " && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'"
        )
        measured = javascript("""const input=document.querySelector('#context-size'),anchor=document.querySelector('.kwic-row'),samples=[];
          input.value=13;input.dispatchEvent(new Event('input'));
          for(let i=0;i<5;i++){
            const context=12+i%2;
            input.value=context;
            const start=performance.now();input.dispatchEvent(new Event('input'));samples.push(performance.now()-start);
            const expected=__pageTemplate.rows[0].left_context.slice(-context).map(token=>token.attributes.text).join('');
            if(anchor!==document.querySelector('.kwic-row')||anchor.querySelector('.context-left>span').textContent!==expected)throw new Error('Context editing changed the occurrence or displayed text');
          }
          return {samples,dom_nodes:document.querySelectorAll('*').length,fixture_bytes:new TextEncoder().encode(JSON.stringify(__measuredPage)).length};
        """)
        results.append(
            {
                "rows": size,
                "rows_per_document": per_document,
                "median_ms": statistics.median(measured["samples"]),
                **measured,
            }
        )
    with http.open(
        urllib.parse.urljoin(args.url, "assets/questions/app.js")
    ) as response:
        asset_digest = hashlib.sha256(response.read()).hexdigest()
    print(
        json.dumps(
            {
                "synthetic": True,
                "browser": started["capabilities"]["browserVersion"],
                "app_sha256": asset_digest,
                "results": results,
            }
        )
    )
finally:
    if session:
        try:
            request("/session/" + session, method="DELETE")
        except (urllib.error.URLError, OSError):
            pass
    driver.terminate()
    driver.wait(timeout=15)

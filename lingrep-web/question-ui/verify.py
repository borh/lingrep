#!/usr/bin/env python3
"""Check the packaged question UI against configured corpus fixtures with Chromium."""

import argparse
import base64
import json
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import urllib.parse
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("url")
parser.add_argument("--output", type=Path)
parser.add_argument("--ling", type=Path)
parser.add_argument("--corpus", type=Path)
parser.add_argument("--author-dataset", default="author")
parser.add_argument("--full-dataset", default="full")
parser.add_argument("--expected-matches", type=int, default=43)
args = parser.parse_args()
query_url = urllib.parse.urlsplit(args.url)
query_base = urllib.parse.urlunsplit((query_url.scheme, query_url.netloc, query_url.path, "", ""))
initial = dict(urllib.parse.parse_qsl(query_url.fragment))
initial.update({"fresh": "1", "run": "0", "corpus": args.author_dataset})
initial_url = query_base + "#" + urllib.parse.urlencode(initial)
out = args.output or Path(tempfile.mkdtemp(prefix="ling-ui-check-"))
out.mkdir(parents=True, exist_ok=True)
out.joinpath("review").mkdir(exist_ok=True)
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
with (out / "browser-driver.log").open("w") as log:
    driver = subprocess.Popen(
        ["chromedriver", "--port=0"], stdout=subprocess.PIPE, stderr=log, text=True
    )
    base = None
    session = None
    try:
        for line in driver.stdout:
            if m := re.search(r"started successfully on port (\d+)", line):
                base = "http://127.0.0.1:" + m[1]
                break

        def req(path, body=None, method=None):
            data = None if body is None else json.dumps(body).encode()
            with http.open(
                urllib.request.Request(
                    base + path,
                    data=data,
                    method=method,
                    headers={"Content-Type": "application/json"},
                ),
                timeout=90,
            ) as response:
                return json.load(response)["value"]

        session = req(
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
        )["sessionId"]
        prefix = "/session/" + session

        def js(source):
            return req(prefix + "/execute/sync", {"script": source, "args": []})

        def wait(source, seconds=60):
            deadline = time.monotonic() + seconds
            while not (value := js(source)):
                assert time.monotonic() < deadline, js("return document.body.innerText")
                time.sleep(0.1)
            return value

        def capture(name, width=1440, height=1100):
            req(prefix + "/window/rect", {"width": width, "height": height})
            req(
                prefix + "/execute/async",
                {
                    "script": "const done=arguments[0];requestAnimationFrame(()=>requestAnimationFrame(done));",
                    "args": [],
                },
            )
            dimensions = js("return {width:innerWidth,height:innerHeight}")
            if dimensions != {"width": width, "height": height}:
                req(
                    prefix + "/window/rect",
                    {
                        "width": width + (width - dimensions["width"]),
                        "height": height + (height - dimensions["height"]),
                    },
                )
            req(
                prefix + "/execute/async",
                {
                    "script": "const done=arguments[0];requestAnimationFrame(()=>requestAnimationFrame(done));",
                    "args": [],
                },
            )
            actual = js("return {width:innerWidth,height:innerHeight}")
            assert actual == {
                "width": width,
                "height": height,
            }, actual
            js("window.scrollTo(0,0)")
            time.sleep(0.2)
            result = js(
                "return {width:innerWidth,height:innerHeight,overflow:document.documentElement.scrollWidth-innerWidth,rows:document.querySelectorAll('.kwic-row').length,status:document.querySelector('#status').textContent,panels:[...document.querySelectorAll('.result-panel')].map(p=>({id:p.dataset.run,rect:p.getBoundingClientRect().toJSON(),title:p.querySelector('.result-header').title})),errors:window.__errors??[]}"
            )
            assert result["overflow"] <= 1, result
            (out / "review" / f"{name}.png").write_bytes(
                base64.b64decode(req(prefix + "/screenshot"))
            )
            return result

        req(
            prefix + "/goog/cdp/execute",
            {
                "cmd": "Page.addScriptToEvaluateOnNewDocument",
                "params": {
                    "source": "window.__errors=[];addEventListener('error',e=>__errors.push(e.message));addEventListener('unhandledrejection',e=>__errors.push(String(e.reason)));"
                },
            },
        )
        req(prefix + "/window/rect", {"width": 1440, "height": 1100})
        req(prefix + "/url", {"url": initial_url})
        wait("return !!window.__probe")
        assert js("return document.querySelector('#error').getAttribute('role')") == "alert"
        assert js("return document.querySelector('#metadata-state').getAttribute('role')") == "status"
        assert js("document.querySelector('#open-question').focus();const s=getComputedStyle(document.querySelector('.file-action'));return s.outlineStyle!=='none' && parseFloat(s.outlineWidth)>0")
        assert js(
            "return !document.querySelector('#run').hidden && document.querySelector('#run').textContent==='Search'"
        )
        assert js(
            "return !document.querySelector('#question') && !document.querySelector('[contenteditable]')"
        )
        assert js(
            "return !!document.querySelector('#edit-source') && !!document.querySelector('#corpus') && !!document.querySelector('#scope-toggle')"
        )
        js(
            "window.__errors=[];addEventListener('error',e=>__errors.push(e.message));addEventListener('unhandledrejection',e=>__errors.push(String(e.reason)));localStorage.setItem('ling-questions-locale','en');document.querySelector('#query-form').requestSubmit()"
        )
        state = wait(
            "return __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        assert state["received"] > 0, state
        first_id = state["id"]
        wait("return document.querySelectorAll('.kwic-row').length===20")
        assert not js("return document.querySelector('#run').hidden")
        js(
            "const transfer=new DataTransfer();transfer.items.add(new File(['{}'],'invalid.json',{type:'application/json'}));const input=document.querySelector('#open-question');input.files=transfer.files;input.dispatchEvent(new Event('change'))"
        )
        wait("return !document.querySelector('#error').hidden")
        assert (
            js(
                "return getComputedStyle(document.querySelector('.search-status')).clipPath"
            )
            == "none"
        )
        assert (
            js("return document.querySelector('#error').getBoundingClientRect().height")
            > 1
        )
        js("document.querySelector('#error').hidden=true")
        assert js(
            "return [...document.querySelectorAll('.kwic-row')].every(row=>row.getAttribute('href').startsWith('#')&&!row.hasAttribute('data-content'))"
        )
        (out / "first-run.json").write_text(
            json.dumps(state, ensure_ascii=False, indent=2)
        )
        with http.open(
            urllib.parse.urljoin(query_base, f"api/questions/runs/{first_id}/export"), timeout=60
        ) as response:
            exported = [json.loads(line) for line in response.read().splitlines()]
        assert exported[0]["type"] == "run" and exported[-1]["type"] == "complete"
        matches = [record["data"] for record in exported if record["type"] == "match"]
        assert (
            len(matches)
            == state["received"]
            == exported[-1]["data"]["counters"]["matches_found"]
        )
        for name, rows in state["rows"].items():
            assert rows == [row for row in matches if row["pattern"] == name][:30]
        assert (
            state["contract_run"]["analysis_identity"]
            == exported[-1]["data"]["analysis_identity"]
        )
        if args.ling and args.corpus:
            query = out / "parity.lx"
            query.write_text(state["source"])
            command = [
                str(args.ling),
                "--json",
                "matches",
                "--corpus",
                str(args.corpus),
                "--patterns",
                str(query),
                "--context",
                "24",
                "--source-metadata",
                "@document.title",
                "--source-metadata",
                "@document.author",
            ]
            independent = subprocess.run(
                command, check=True, capture_output=True, text=True
            )
            expected = [
                record["data"]
                for line in independent.stdout.splitlines()
                if (record := json.loads(line))["type"] == "match"
            ]
            assert matches == expected, (
                "Question export differs from a separate CLI query"
            )
        js("document.querySelector('#line-limit').value=100;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return document.querySelectorAll('.kwic-row').length===" + str(args.expected_matches))
        assert js("return Number(document.querySelector('#line-limit').max)") == args.expected_matches
        assert js("return __probe.state.current.id") == first_id
        js("document.querySelector('#context-size').value=9999999;document.querySelector('#context-size').dispatchEvent(new Event('input'))")
        assert js("return document.querySelector('#context-size').value") == "24"
        js("document.querySelector('#line-limit').value=9999999;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        assert js("return Number(document.querySelector('#line-limit').value)") == args.expected_matches
        js("document.querySelector('#context-size').value=12;document.querySelector('#context-size').dispatchEvent(new Event('input'))")
        for selector, state_key in [("#context-size", "context"), ("#line-limit", "lines")]:
            previous = js("return __probe.controls.state." + state_key)
            js("const input=document.querySelector(" + json.dumps(selector) + ");input.focus();input.value=1.5;input.dispatchEvent(new Event('input'))")
            assert js("return document.querySelector('#reading-validation').hidden")
            js("document.querySelector(" + json.dumps(selector) + ").dispatchEvent(new Event('change'))")
            assert js("return !document.querySelector('#reading-validation').hidden")
            assert js("return document.querySelector(" + json.dumps(selector) + ").getAttribute('aria-invalid')") == "true"
            js("document.querySelector('#behavior').dispatchEvent(new Event('change'))")
            assert js("return __probe.controls.state." + state_key) == previous
            js("document.querySelector('#language').click()")
            assert "整数" in js("return document.querySelector('#reading-validation').textContent")
            js("document.querySelector('#language').click()")
            assert "whole number" in js("return document.querySelector('#reading-validation').textContent")
            js("__probe.controls.restore(new URLSearchParams(__probe.controls.entries()))")
            assert js("return document.querySelector('#reading-validation').hidden && !document.querySelector(" + json.dumps(selector) + ").hasAttribute('aria-invalid')")
            js("const input=document.querySelector(" + json.dumps(selector) + ");input.value=" + str(previous) + ";input.dispatchEvent(new Event('input'))")
            assert js("return document.querySelector('#reading-validation').hidden")
        js("window.__contextRow=document.querySelector('.kwic-row');window.__contextText=__contextRow.querySelector('.context-left>span').firstChild;window.__headingLink=document.querySelector('.document-heading a');document.querySelector('#context-size').value=4;document.querySelector('#context-size').dispatchEvent(new Event('input'))")
        assert js("return __contextRow===document.querySelector('.kwic-row') && __contextText===__contextRow.querySelector('.context-left>span').firstChild && __headingLink===document.querySelector('.document-heading a')")
        js("document.querySelector('#context-size').value=12;document.querySelector('#context-size').dispatchEvent(new Event('input'))")
        js("document.querySelector('#line-limit').value=20;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return document.querySelectorAll('.kwic-row').length===20 && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'")
        js("document.querySelector('#line-limit').value=5;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return document.querySelectorAll('.kwic-row').length===5 && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'")
        js("window.__pageFetch=window.fetch;window.__pageFailure=true;window.fetch=(url,...args)=>{if(__pageFailure&&String(url).includes('/documents?')){__pageFailure=false;return new Promise((resolve,reject)=>window.__rejectPage=()=>reject(new Error('Example page interrupted')));}return __pageFetch(url,...args)};document.querySelector('#line-limit').value=100;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return !!window.__rejectPage")
        assert js("return document.querySelector('.result-reading .line-note')?.textContent") == "Loading examples…"
        js("__rejectPage()")
        wait("return document.querySelector('.line-note').textContent.includes('could not')")
        js("document.querySelector('#line-limit').value=25;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return document.querySelectorAll('.kwic-row').length===25 && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'")
        assert js("return __probe.state.current.id") == first_id
        js("window.__holdPage=true;window.fetch=(url,...args)=>{if(__holdPage&&String(url).includes('/documents?')){__holdPage=false;return new Promise(resolve=>window.__releaseCountPage=()=>resolve(__pageFetch(url,...args)));}return __pageFetch(url,...args)};document.querySelector('#line-limit').value=30;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return !!window.__releaseCountPage")
        js("for(const value of [35,40]){document.querySelector('#line-limit').value=value;document.querySelector('#line-limit').dispatchEvent(new Event('input'))}__releaseCountPage()")
        wait("return document.querySelectorAll('.kwic-row').length===40 && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'")
        js("window.fetch=__pageFetch;document.querySelector('#line-limit').value=20;document.querySelector('#line-limit').dispatchEvent(new Event('input'))")
        wait("return document.querySelectorAll('.kwic-row').length===20 && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'")
        wait("return document.querySelectorAll('.document-nav select option').length>2")
        inventory = js(
            "return [...document.querySelectorAll('.document-nav select option')].slice(1).map(o=>({id:Number(o.value),title:o.textContent}))"
        )
        expected_docs = list(dict.fromkeys(row["document_ordinal"] for row in matches))
        assert [d["id"] for d in inventory] == expected_docs
        visited = []
        while True:
            page = wait(
                "return document.querySelector('.kwic-row') && [...document.querySelectorAll('.kwic-row')].map(row=>Number(row.dataset.sequence))"
            )
            visited.extend(page)
            assert (
                js(
                    "return document.querySelector('.line-controls input').getAttribute('aria-label')"
                )
                == "Filter this page"
            )
            link = js(
                "return document.querySelector('[data-page-step=\"1\"]').getAttribute('href')"
            )
            if not link:
                break
            previous_first = page[0]
            js("document.querySelector('[data-page-step=\"1\"]').click()")
            wait(
                "return document.querySelector('.kwic-row') && Number(document.querySelector('.kwic-row').dataset.sequence)!="
                + str(previous_first)
            )
        assert visited == [row["sequence"] for row in matches]
        js(
            "window.__inspectionRequests=[];window.__inspectionFetch=window.fetch;window.fetch=(input,init)=>{__inspectionRequests.push(String(input));return __inspectionFetch(input,init)};document.querySelector('.kwic-row').click()"
        )
        wait("return document.querySelector('#token-ribbon .selected')")
        assert js(
            "return !document.querySelector('.attribute-sentence') && !document.querySelector('#attribute-key')"
        )
        assert (
            js(
                "return __inspectionRequests.filter(url=>url.includes('/documents?')).length"
            )
            == 0
        )
        assert (
            js(
                "return __inspectionRequests.filter(url=>url.includes('/tokens?')).length"
            )
            == 1
        )
        assert (
            js("return document.querySelector('#token-lab').getAnimations().length")
            == 0
        )
        js(
            "window.fetch=window.__inspectionFetch;document.querySelector('#close-token').click()"
        )
        wait("return document.querySelector('#token-lab').hidden")
        js(
            "document.querySelector('.document-nav select').value='';document.querySelector('.document-nav select').dispatchEvent(new Event('change'))"
        )
        wait(
            "return document.querySelector('.kwic-row') && Number(document.querySelector('.kwic-row').dataset.sequence)=="
            + str(matches[0]["sequence"])
        )
        label = js(
            "return document.querySelector('.kwic-row').getAttribute('aria-label')"
        )
        assert matches[0]["surface"] in label
        assert (
            "".join(token["attributes"]["text"] for token in matches[0]["left_context"])
            in label
        )
        last = inventory[-1]["id"]
        js(
            "document.querySelector('.document-nav select').value='"
            + str(last)
            + "';document.querySelector('.document-nav select').dispatchEvent(new Event('change'))"
        )
        wait(
            "return document.querySelector('.kwic-row') && !document.querySelector('.document-nav').textContent.includes('Loading')"
        )
        assert all(
            row["document_ordinal"] == last
            for row in js(
                "return [...document.querySelectorAll('.kwic-row')].map(b=>Number(b.dataset.sequence))"
            )
            for row in [next(r for r in matches if r["sequence"] == row)]
        )
        assert (
            js("return Number(document.querySelector('.kwic-row').dataset.sequence)")
            > 30
        )
        metrics = {"document": capture("document")}
        js(
            "window.__fetch=window.fetch;window.__failedDocuments=false;window.fetch=(url,...args)=>{if(!__failedDocuments&&String(url).includes('/documents?')){__failedDocuments=true;return Promise.reject(new Error('Document connection interrupted'));}return __fetch(url,...args);};document.querySelector('.document-nav select').selectedIndex=document.querySelector('.document-nav select').options.length-2;document.querySelector('.document-nav select').dispatchEvent(new Event('change'))"
        )
        wait("return document.querySelector('[data-document-retry]')?.hidden===false")
        metrics["document-retry"] = capture("document-retry")
        js(
            "window.fetch=window.__fetch;document.querySelector('[data-document-retry]').click()"
        )
        wait(
            "return document.querySelector('[data-document-retry]').hidden && document.querySelector('.kwic-row')"
        )
        assert js("return __probe.state.current.id") == first_id

        js(
            "document.querySelector('#line-limit').value=3;document.querySelector('#line-limit').dispatchEvent(new Event('input'));document.querySelector('#context-size').value=4;document.querySelector('#context-size').dispatchEvent(new Event('input'))"
        )
        assert (
            js(
                "return __probe.controls.state.lines===3 && __probe.controls.state.context===4 && __probe.state.current.id"
            )
            == first_id
        )
        assert js("return document.querySelectorAll('.kwic-row').length") <= 3
        point = js(
            "const e=document.querySelector('#context-size');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return {x:Math.round(r.x+r.width/2),y:Math.round(r.y+r.height/2)}"
        )
        req(
            prefix + "/actions",
            {
                "actions": [
                    {
                        "type": "pointer",
                        "id": "number-drag",
                        "parameters": {"pointerType": "mouse"},
                        "actions": [
                            {
                                "type": "pointerMove",
                                "duration": 0,
                                "x": point["x"],
                                "y": point["y"],
                            },
                            {"type": "pointerDown", "button": 0},
                            {
                                "type": "pointerMove",
                                "duration": 150,
                                "x": point["x"] + 16,
                                "y": point["y"],
                            },
                            {"type": "pointerUp", "button": 0},
                        ],
                    }
                ]
            },
        )
        assert js("return __probe.controls.state.context") == 6
        assert js("return __probe.state.current.id") == first_id
        js(
            "document.querySelector('#context-size').value=4;document.querySelector('#context-size').dispatchEvent(new Event('input'))"
        )
        js(
            "window.__metadataFetch=window.fetch;window.fetch=(url,...options)=>String(url).includes('/metadata')?new Promise(resolve=>{window.__heldMetadataUrl=String(url);window.__releaseMetadata=()=>resolve(__metadataFetch(url,...options))}):__metadataFetch(url,...options);document.querySelector('#scope-toggle').click()"
        )
        wait("return !!window.__releaseMetadata")
        assert js("return document.querySelector('#metadata-picker').hidden")
        assert (
            js(
                "return document.querySelector('#scope-toggle').getAttribute('aria-busy')"
            )
            == "true"
        )
        js("__releaseMetadata()")
        wait("return window.__heldMetadataUrl.includes('/metadata?')")
        assert js("return document.querySelector('#metadata-picker').hidden")
        js("window.fetch=__metadataFetch;__releaseMetadata()")
        wait(
            "return !document.querySelector('#metadata-picker').hidden && document.querySelector('.metadata-value')"
        )
        assert js(
            "return !document.querySelector('#scope-toggle').hasAttribute('aria-busy')"
        )
        js(
            "window.__priorMetadataValue=document.querySelector('.metadata-value');window.fetch=(url,...options)=>String(url).includes('/metadata?')?new Promise(resolve=>window.__releaseValues=()=>resolve(__metadataFetch(url,...options))):__metadataFetch(url,...options);document.querySelector('#metadata-field').value='ndc';document.querySelector('#metadata-field').dispatchEvent(new Event('change'))"
        )
        wait("return !!window.__releaseValues")
        assert js(
            "return __priorMetadataValue.isConnected && document.querySelector('#metadata-values').inert && document.querySelector('#metadata-values').getAttribute('aria-busy')==='true'"
        )
        js("window.fetch=__metadataFetch;__releaseValues()")
        wait(
            "return document.querySelector('#metadata-values').textContent.includes('日本文学')"
        )
        metrics["ndc"] = capture("ndc")
        js(
            "document.querySelector('#metadata-query').value='小説';document.querySelector('#metadata-query').dispatchEvent(new Event('input'))"
        )
        wait(
            "return [...document.querySelectorAll('.metadata-value')].some(e=>e.textContent.includes('913 日本文学'))"
        )
        js(
            "[...document.querySelectorAll('.metadata-value')].find(e=>e.textContent.includes('913 日本文学')).querySelector('input').click()"
        )
        filtered = wait(
            "return __probe.state.current?.id!=='"
            + first_id
            + "' && __probe.state.current.phase==='complete' && __probe.state.current"
        )
        assert (
            'contains "913"' in filtered["where"]
            and "日本文学" not in filtered["where"]
        )
        assert "日本文学" in js(
            "return document.querySelector('#scope-chips').textContent"
        )
        js(
            "document.querySelector('#metadata-field').value='source_edition_year';document.querySelector('#metadata-field').dispatchEvent(new Event('change'))"
        )
        wait("return document.querySelector('[data-bound=min]')")
        assert js("return ![...document.querySelector('#metadata-field').options].some(option=>['identifier','filename','source_content_hash','source_bundle_hash','release','url','doi'].includes(option.value))")
        year_element = req(prefix + "/element", {"using":"css selector","value":"[data-bound=min]"})["element-6066-11e4-a52e-4f735466cecf"]
        req(prefix + "/element/" + year_element + "/value", {"text":"\ue009a\ue0001"})
        assert js("return document.querySelector('#metadata-validation').hidden")
        req(prefix + "/element/" + year_element + "/value", {"text":"940"})
        assert js("return document.querySelector('[data-bound=min]').value") == "1940"
        js("window.__validMetadata=JSON.stringify(__probe.controls.state.metadata);const input=document.querySelector('[data-bound=min]');input.value=1940.5;input.dispatchEvent(new Event('input'));input.dispatchEvent(new Event('change'))")
        assert js("return !document.querySelector('#metadata-validation').hidden && document.querySelector('[data-bound=min]').getAttribute('aria-invalid')==='true' && document.querySelector('[data-bound=min]').getAttribute('aria-describedby').includes('metadata-validation')")
        assert js("return JSON.stringify(__probe.controls.state.metadata)") == js("return __validMetadata")
        js("const input=document.querySelector('[data-bound=min]');input.value=1940;input.dispatchEvent(new Event('input'))")
        assert js("return document.querySelector('#metadata-validation').hidden")
        js(
            "document.querySelector('[data-bound=max]').value=1960;document.querySelector('[data-bound=max]').dispatchEvent(new Event('input'));document.querySelector('[data-bound=min]').value=1940;document.querySelector('[data-bound=min]').dispatchEvent(new Event('input'))"
        )
        wait(
            "return __probe.state.current?.where.includes('>= 1940') && __probe.state.current.where.includes('<= 1960') && __probe.state.current.phase==='complete'"
        )
        js(
            "document.querySelector('.range-window').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}))"
        )
        wait(
            "return __probe.state.current?.where.includes('>= 1941') && __probe.state.current.where.includes('<= 1961') && __probe.state.current.phase==='complete'"
        )
        js(
            "document.querySelector('[data-bound=min]').value=2000;document.querySelector('[data-bound=min]').dispatchEvent(new Event('input'))"
        )
        assert js("return document.querySelector('[data-bound=min]').value === document.querySelector('[data-bound=max]').value && document.querySelector('#metadata-validation').hidden")
        js(
            "document.querySelector('[data-bound=min]').value=1941;document.querySelector('[data-bound=min]').dispatchEvent(new Event('input'))"
        )
        metrics["range"] = capture("range")
        js(
            "document.querySelector('#metadata-close').click();document.querySelector('#metadata-clear').click()"
        )
        wait(
            "return __probe.state.current?.where==='' && __probe.state.current.phase==='complete'"
        )
        js(
            "document.querySelector('#line-limit').value=20;document.querySelector('#line-limit').dispatchEvent(new Event('input'));document.querySelector('#context-size').value=12;document.querySelector('#context-size').dispatchEvent(new Event('input'))"
        )
        wait("return document.querySelectorAll('.kwic-row').length===20")
        js("window.__inspectionTarget=document.querySelector('.kwic-row');__inspectionTarget.click()")
        wait("return __inspectionTarget.getAttribute('aria-expanded')==='true' && !__inspectionTarget.hasAttribute('aria-busy') && document.querySelector('#token-lab').previousElementSibling===__inspectionTarget && document.querySelectorAll('#token-ribbon button').length>0")
        selected = js(
            "return document.querySelector('#token-ribbon .selected').dataset.token"
        )
        js(
            "const other=[...document.querySelectorAll('#token-ribbon button')].find(b=>!b.classList.contains('selected'));other.dispatchEvent(new PointerEvent('pointerenter'))"
        )
        assert (
            js("return document.querySelector('#token-ribbon .selected').dataset.token")
            == selected
        )
        js(
            "const other=[...document.querySelectorAll('#token-ribbon button')].find(b=>!b.classList.contains('selected'));other.click()"
        )
        assert (
            js("return document.querySelector('#token-ribbon .selected').dataset.token")
            != selected
        )
        js(
            "window.__readerFetch=window.fetch;window.__readerFailed=false;window.fetch=(url,...args)=>{if(!__readerFailed&&String(url).includes('api/read/')){__readerFailed=true;return Promise.resolve(new Response('Reader is preparing',{status:503}));}return __readerFetch(url,...args)}"
        )
        js("document.querySelector('#document').click()")
        wait("return document.querySelector('[data-read-retry]')")
        js(
            "window.fetch=window.__readerFetch;document.querySelector('[data-read-retry]').click()"
        )
        wait(
            "return document.querySelector('#document-body mark') && document.querySelector('#document-body').getAttribute('aria-busy')==='false'"
        )
        origin = js("return document.querySelector('#inspect-title').textContent")
        js(
            "window.__readerNode=document.querySelector('#document-body').firstElementChild;window.__readerTitle=document.querySelector('#inspect-title').textContent;window.__pageReadFetch=window.fetch;window.__readHeld=false;window.fetch=(url,...args)=>{if(!__readHeld&&String(url).includes('api/read/')){__readHeld=true;return new Promise(resolve=>window.__releaseReader=()=>resolve(__pageReadFetch(url,...args)));}return __pageReadFetch(url,...args)}"
        )
        js("document.querySelector('[data-reader-document=\"1\"]').click()")
        wait("return !!window.__releaseReader")
        assert js(
            "return document.querySelector('#document-body').firstElementChild===__readerNode && document.querySelector('#inspect-title').textContent===__readerTitle"
        )
        assert js(
            "return [...document.querySelectorAll('#document-body button')].every(button=>button.disabled)"
        )
        js("__releaseReader();window.fetch=__pageReadFetch")
        wait(
            "return document.querySelector('#inspect-title').textContent!=="
            + json.dumps(origin)
            + " && document.querySelector('#document-body mark')"
        )
        metrics["reader"] = capture("reader-document")
        js(
            "document.querySelector('#close-inspector').click();document.querySelector('#close-token').click()"
        )
        first_id = js("return __probe.state.current.id")
        js(
            "document.querySelector('.document-nav select').value='';document.querySelector('.document-nav select').dispatchEvent(new Event('change'))"
        )
        wait("return document.querySelectorAll('.kwic-row').length===20")
        assert js("return !document.querySelector('#density')")
        metrics.update(
            {"desktop": capture("desktop"), "mobile": capture("mobile", 390, 844)}
        )
        js("document.querySelector('#scope-toggle').click()")
        wait("return document.querySelectorAll('#metadata-field option').length>0")
        metrics["metadata"] = capture("metadata", 1440, 1100)
        js("document.querySelector('#metadata-close').click()")
        req(prefix + "/window/rect", {"width": 1440, "height": 1100})
        js("document.querySelector('.kwic-row').click()")
        wait("return document.querySelectorAll('#token-ribbon button').length>0")
        metrics["tokens"] = capture("tokens")
        js("document.querySelector('#document').click()")
        wait(
            "return document.querySelector('#document-body').textContent.length>100 && !document.querySelector('#document-body').textContent.includes('Loading')"
        )
        metrics["reader"] = capture("reader")
        assert "Could not load" not in js(
            "return document.querySelector('#document-body').textContent"
        )
        assert (
            js(
                "return document.querySelector('#document-body').querySelectorAll('mark').length"
            )
            > 0
        )
        js(
            "document.querySelector('#close-inspector').click();document.querySelector('#close-token').click();document.querySelector('#source').value='かもしれない := か も しれ ない.';document.querySelector('#source').dispatchEvent(new Event('input'));document.querySelector('#corpus').value="
            + json.dumps(args.full_dataset)
            + ";document.querySelector('#corpus').dispatchEvent(new Event('change'))"
        )
        wait(
            "return __probe.state.current?.dataset===" + json.dumps(args.full_dataset) + " && __probe.state.current.phase==='running'"
        )
        wait(
            "return __probe.state.current.received>0 && __probe.state.current.phase==='running'"
        )
        assert (
            js("return document.querySelector('.result-panel').dataset.run") == first_id
        )
        js(
            "window.__download=document.querySelector('.result-header a');__download.focus();"
        )
        time.sleep(0.6)
        assert js(
            "return document.activeElement===__download && __download.isConnected"
        )
        metrics["keep"] = capture("keep-previous")
        js(
            "document.querySelector('#behavior').value='live';document.querySelector('#behavior').dispatchEvent(new Event('change'))"
        )
        wait(
            "return document.querySelector('.result-panel').dataset.run===__probe.state.current.id"
        )
        metrics["live"] = capture("live")
        js(
            "document.querySelector('#source').value='submitted := か も しれ ない.';document.querySelector('#source').dispatchEvent(new Event('input'));__probe.search();document.querySelector('#source').value='draft := と いう.';document.querySelector('#source').dispatchEvent(new Event('input'))"
        )
        submitted = wait(
            "return __probe.state.current?.source==='submitted := か も しれ ない.' && __probe.state.current.phase==='running' && __probe.state.current"
        )
        assert (
            js("return document.querySelector('#source').value") == "draft := と いう."
        )
        req(prefix + "/refresh", {})
        wait("return window.__probe?.state.current?.id==='" + submitted["id"] + "'")
        assert (
            js("return __probe.state.current.source") == "submitted := か も しれ ない."
        )
        assert (
            js("return document.querySelector('#source').value") == "draft := と いう."
        )
        assert js("return __probe.state.completed.id") == first_id
        wait("return __probe.state.current.received>0")
        js(
            "document.querySelector('#behavior').value='compare';document.querySelector('#behavior').dispatchEvent(new Event('change'))"
        )
        wait("return document.querySelectorAll('.result-panel').length===2")
        metrics["compare"] = capture("compare")
        metrics["mobile-compare"] = capture("mobile-compare", 390, 844)
        js("document.querySelector('#cancel').click()")
        stopped = wait(
            "return __probe.state.current?.phase==='cancelled' && __probe.state.current"
        )
        assert js("return __probe.state.completed.id") == first_id
        js(
            "document.querySelector('#behavior').value='keep';document.querySelector('#behavior').dispatchEvent(new Event('change'));document.querySelector('#language').click()"
        )
        metrics["mobile-ja"] = capture("mobile-ja", 390, 844)
        js(
            "document.querySelector('#source').value='broken := [';document.querySelector('#source').dispatchEvent(new Event('input'));document.querySelector('#query-form').requestSubmit()"
        )
        wait("return __probe.state.current?.phase==='failed'")
        assert (
            js("return document.querySelector('.result-panel').dataset.run") == first_id
        )
        metrics["error"] = capture("error", 1440, 1100)
        js("document.querySelector('#replay').click()")
        wait("return !document.querySelector('#replay-note').hidden")
        js("document.querySelector('#cancel').click()")
        wait("return document.querySelector('#replay-note').hidden")
        assert js("return __probe.state.current.phase") == "complete"
        js(
            "document.querySelector('#source-editor').hidden=true;document.querySelector('#edit-source').setAttribute('aria-expanded','false')"
        )
        metrics["mobile-final"] = capture("mobile-final", 390, 844)
        req(
            prefix + "/goog/cdp/execute",
            {
                "cmd": "Emulation.setEmulatedMedia",
                "params": {
                    "features": [{"name": "prefers-color-scheme", "value": "dark"}]
                },
            },
        )
        metrics["dark"] = capture("dark", 1440, 1100)
        req(
            prefix + "/goog/cdp/execute",
            {"cmd": "Emulation.setEmulatedMedia", "params": {"features": []}},
        )
        js("document.body.style.zoom=2")
        metrics["scaled"] = capture("scaled", 1440, 1100)
        js("document.body.style.zoom=1")
        assert not js("return window.__errors??[]"), js("return window.__errors??[]")
        req(prefix + "/window/rect", {"width": 390, "height": 844})
        js("localStorage.setItem('ling-questions-locale','en')")
        req(prefix + "/url", {"url": args.url.split("#")[0] + "#fresh=1"})
        req(prefix + "/refresh", {})
        wait("return !!window.__probe")
        js("document.querySelector('#query-form').requestSubmit()")
        complete = wait(
            "return __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        wait("return document.querySelector('.kwic-row')")
        capture("hypertext-mobile", 390, 844)
        mobile = js(
            "return {top:document.querySelector('.kwic-row').getBoundingClientRect().top,open:document.querySelector('#reading-options').open,actions:document.querySelector('#question-actions').open}"
        )
        assert mobile["top"] < 450, mobile
        assert mobile["open"] and not mobile["actions"], mobile
        js("document.querySelector('#question-actions').open=true")
        assert js(
            "return document.querySelector('#save-question').getBoundingClientRect().height>0"
        )
        js(
            "document.querySelector('#question-actions').open=false;document.querySelector('#reading-options').open=true"
        )
        size = js(
            "const r=document.querySelector('#line-limit').getBoundingClientRect();return {w:r.width,h:r.height}"
        )
        assert size["h"] >= 44 and size["w"] >= 44, size
        req(prefix + "/window/rect", {"width": 1440, "height": 1100})
        js("document.querySelector('.kwic-row').click()")
        wait("return document.querySelector('#token-ribbon .selected')")
        selected = js(
            "return document.querySelector('#token-ribbon .selected').dataset.token"
        )
        js(
            "document.querySelector('#token-ribbon button').dispatchEvent(new PointerEvent('pointerenter'))"
        )
        assert (
            js("return document.querySelector('#token-ribbon .selected').dataset.token")
            == selected
        )
        link = js("return document.querySelector('#document').href")
        js("document.querySelector('#document').click()")
        wait(
            "return document.querySelector('#document-body mark') && document.querySelector('#document-body').getAttribute('aria-busy')==='false'"
        )
        assert js(
            "return document.querySelector('#inspector').open && !document.querySelector('#inspector').matches(':modal')"
        )
        capture("inline-reader")
        js("document.querySelector('#close-inspector').click()")
        wait("return !document.querySelector('#inspector').open")
        assert js("return !document.querySelector('#token-lab').hidden")
        original_window = req(prefix + "/window")
        new_window = req(prefix + "/window/new", {"type": "tab"})["handle"]
        req(prefix + "/window", {"handle": new_window})
        req(prefix + "/url", {"url": link})
        wait(
            "return document.querySelector('#document-body mark') && document.querySelector('#document-body').getAttribute('aria-busy')==='false'"
        )
        assert js("return __probe.state.current.id") == complete["id"]
        req(prefix + "/window", method="DELETE")
        req(prefix + "/window", {"handle": original_window})
        req(prefix + "/url", {"url": link})
        wait(
            "return document.querySelector('#document-body mark') && document.querySelector('#document-body').getAttribute('aria-busy')==='false'"
        )
        assert js("return __probe.state.current.id") == complete["id"]
        req(prefix + "/back", {})
        wait("return !document.querySelector('#inspector').open")
        req(prefix + "/forward", {})
        wait(
            "return document.querySelector('#inspector').open && document.querySelector('#document-body mark')"
        )
        js(
            "document.querySelector('#close-inspector').click();document.querySelector('#close-token').click()"
        )
        source = "valid := [text=猫😀].\nbroken := [upos=VERB."
        js(
            "document.querySelector('#source').value="
            + json.dumps(source)
            + ";document.querySelector('#source').dispatchEvent(new Event('input'));document.querySelector('#query-form').requestSubmit()"
        )
        failed = wait(
            "return __probe.state.current?.phase==='failed' && __probe.state.current"
        )
        wait("return !document.querySelector('#edit-pattern-error').hidden")
        diag = next(
            d["data"] for d in failed["diagnostics"] if d["data"]["stage"] == "catalog"
        )
        span = diag["location"]["source_span"]
        expected = [
            len(source.encode()[: span[key]].decode().encode("utf-16-le")) // 2
            for key in ["start", "end"]
        ]
        assert (
            js(
                "return [document.querySelector('#source').selectionStart,document.querySelector('#source').selectionEnd]"
            )
            == expected
        )
        assert js("return !document.querySelector('#source-editor').hidden")
        assert (
            js("return document.querySelector('.result-panel').dataset.run")
            == complete["id"]
        )
        capture("syntax-recovery")
        js(
            "document.querySelector('#source').value='my newer draft';document.querySelector('#edit-pattern-error').click()"
        )
        assert js("return document.querySelector('#source').value") == "my newer draft"
        assert not js("return window.__errors"), js("return window.__errors")
        metrics["hypertext"] = {
            "mobile": mobile,
            "numeric_target": size,
            "diagnostic_line": diag["location"]["line"],
        }
        assert diag["location"]["line"] == 2
        js("localStorage.setItem('ling-questions-locale','en')")
        req(prefix + "/url", {"url": args.url.split("#")[0] + "#fresh=1"})
        req(prefix + "/refresh", {})
        wait("return !!window.__probe")
        js("document.querySelector('#query-form').requestSubmit()")
        complete = wait(
            "return __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        wait("return document.querySelectorAll('.kwic-row').length===20")
        assert (
            js(
                "return [...document.querySelectorAll('.document-choice [data-document-step]')].filter(link=>!link.hidden).length"
            )
            == 0
        )
        wait(
            "return document.querySelector('.document-heading .document-match-count')?.textContent==='2 matches'"
        )
        js(
            "window.__first=document.querySelector('.kwic-row');window.__second=document.querySelectorAll('.kwic-row')[1];window.__removed=0;window.__emptyFrames=0;window.__watch=true;window.__observer=new MutationObserver(records=>{for(const record of records)for(const node of record.removedNodes)if(node.nodeType===1&&node.matches('.kwic-row'))__removed++;});__observer.observe(document.querySelector('.kwic-list'),{childList:true});function watch(){if(__watch){if(!document.querySelector('.kwic-row'))__emptyFrames++;requestAnimationFrame(watch)}}requestAnimationFrame(watch);__first.click()"
        )
        wait("return document.querySelector('#token-ribbon .selected')")
        assert js(
            "return __first===document.querySelector('.kwic-row') && __second===document.querySelectorAll('.kwic-row')[1] && __removed===0"
        )
        assert js(
            "return __first.nextElementSibling===document.querySelector('#token-lab') && document.querySelector('#token-lab').nextElementSibling===__second"
        )
        js(
            "document.querySelector('#attribute-remix').click();document.querySelector('[data-attribute=\"upos\"]').click();document.querySelector('#attribute-remix').click()"
        )
        assert js("return document.querySelectorAll('.test-builder li').length") == 2
        assert " & " in js(
            "return document.querySelector('#pattern-preview').textContent"
        )
        js(
            "document.querySelector('#token-ribbon .selected').nextElementSibling.click();document.querySelector('[data-attribute=\"lemma\"]').click();document.querySelector('#attribute-remix').click()"
        )
        assert js("return document.querySelectorAll('.test-builder li').length") == 3
        source = js("return document.querySelector('#pattern-preview').textContent")
        capture("composed-analysis")
        previous_origin = js(
            "return document.querySelector('#token-origin').textContent"
        )
        js(
            "window.__tokenFetch=window.fetch;window.fetch=(url,...args)=>String(url).includes('/tokens?')?Promise.reject(new Error('Token connection interrupted')):__tokenFetch(url,...args);__second.click()"
        )
        wait("return document.querySelector('.analysis-error')")
        assert (
            js("return document.querySelector('#token-origin').textContent")
            == previous_origin
        )
        assert (
            js("return document.querySelector('#pattern-preview').textContent")
            == source
        )
        assert js(
            "return __first.nextElementSibling===document.querySelector('#token-lab')"
        )
        js(
            "window.fetch=__tokenFetch;document.querySelector('.analysis-error button').click()"
        )
        wait(
            "return !document.querySelector('.analysis-error') && document.querySelector('#token-origin').textContent!=="
            + json.dumps(previous_origin)
        )
        assert js("return document.querySelector('.test-builder').hidden")
        js("window.__returnFocusRow=document.querySelector('.kwic-row[aria-expanded=true]');document.querySelector('#close-token').focus();document.querySelector('#close-token').click()")
        wait("return document.querySelector('#token-lab').hidden && document.activeElement===__returnFocusRow")
        assert js(
            "return __first===document.querySelector('.kwic-row') && __second===document.querySelectorAll('.kwic-row')[1] && __removed===0"
        )
        js(
            "__observer.disconnect();document.querySelector('.document-heading a').click()"
        )
        wait(
            "return !document.querySelector('.document-scope').hidden && document.querySelectorAll('.kwic-row').length===2"
        )
        assert (
            js(
                "return [...document.querySelectorAll('.document-choice [data-document-step]')].filter(link=>!link.hidden).length"
            )
            == 2
        )
        assert js("return document.querySelector('.evidence-sentence').textContent") == "2 matches in 1 document."
        capture("work-scoped")
        js(
            "window.__scoped=[...document.querySelectorAll('.kwic-row')];window.__oldFetch=window.fetch;window.__held=false;window.fetch=(url,...args)=>{if(!__held&&String(url).includes('/documents?')){__held=true;return new Promise(resolve=>window.__releasePage=()=>resolve(__oldFetch(url,...args)));}return __oldFetch(url,...args)};document.querySelector('[data-document-step=\"1\"]').click()"
        )
        wait("return !!window.__releasePage")
        assert js(
            "return __scoped.every((row,index)=>row===document.querySelectorAll('.kwic-row')[index]) && document.querySelector('.kwic-list').getAttribute('aria-busy')==='true'"
        )
        js("__releasePage();window.fetch=__oldFetch")
        wait("return document.querySelectorAll('.kwic-row').length===11")
        assert (
            js(
                "return document.querySelector('.document-heading .document-match-count').textContent"
            )
            == "11 matches"
        )
        js(
            "window.__scopeFetch=window.fetch;window.fetch=(url,...args)=>String(url).includes('/documents?')?Promise.reject(new Error('Scope connection interrupted')):__scopeFetch(url,...args)"
        )
        js("document.querySelector('.document-scope a').click()")
        wait("return document.querySelector('[data-page-retry]')?.hidden===false")
        assert js("return document.querySelectorAll('.kwic-row').length") == 11
        js(
            "window.fetch=__scopeFetch;document.querySelector('[data-page-retry]').click()"
        )
        wait(
            "return document.querySelector('[data-page-retry]').hidden && document.querySelector('.kwic-list').getAttribute('aria-busy')==='false'"
        )
        assert js("return document.querySelector('.document-scope').hidden"), (
            "Retry lost the requested unscoped destination"
        )
        wait(
            "return document.querySelector('.document-scope').hidden && document.querySelectorAll('.kwic-row').length===20"
        )
        assert js("return window.__emptyFrames") == 0
        js(
            "window.__pager=document.querySelector('[data-page-step=\"1\"]');__pager.focus();__pager.click()"
        )
        assert js("return __pager.isConnected && document.activeElement===__pager")
        wait(
            "return document.querySelector('.line-controls span').textContent.includes('21–40')"
        )
        assert js("return __pager.isConnected && document.activeElement===__pager")
        js("document.querySelector('[data-page-step=\"-1\"]').click()")
        wait(
            "return document.querySelector('.line-controls span').textContent.includes('1–20')"
        )
        js(
            "window.__place=document.querySelectorAll('.kwic-row')[12];__place.scrollIntoView({block:'center'});window.__placeTop=__place.getBoundingClientRect().top;__place.click()"
        )
        wait(
            "return document.querySelector('#token-lab').previousElementSibling===__place && !document.querySelector('#token-lab').getAnimations().length"
        )
        assert (
            js("return Math.abs(__place.getBoundingClientRect().top-__placeTop)") <= 1
        )
        js("document.querySelector('#close-token').click();window.scrollTo(0,0)")
        previous_id = complete["id"]
        js(
            "window.__beforeReplacement=document.querySelector('.kwic-row');window.__replacementFetch=window.fetch;window.__replacementHeld=false;window.fetch=(url,...args)=>{if(!__replacementHeld&&String(url).includes('/documents?')&&!String(url).includes('"
            + previous_id
            + "')){__replacementHeld=true;return new Promise(resolve=>window.__releaseReplacement=()=>resolve(__replacementFetch(url,...args)));}return __replacementFetch(url,...args)};document.querySelector('#source').value='changed := か も しれ ない.';document.querySelector('#source').dispatchEvent(new Event('input'));document.querySelector('#query-form').requestSubmit()"
        )
        replacement = wait(
            "return __probe.state.current?.phase==='complete' && __probe.state.current.id!=='"
            + previous_id
            + "' && window.__releaseReplacement && __probe.state.current"
        )
        assert (
            js("return document.querySelector('.result-panel').dataset.run")
            == previous_id
        )
        assert js(
            "return document.querySelector('.kwic-row')===window.__beforeReplacement"
        )
        js("__releaseReplacement();window.fetch=__replacementFetch")
        wait(
            "return document.querySelector('.result-panel').dataset.run==='"
            + replacement["id"]
            + "' && document.querySelectorAll('.kwic-row').length===20"
        )
        assert js("return window.__emptyFrames") == 0
        js("window.__watch=false")
        capture("stable-concordance")
        capture("stable-mobile", 390, 844)
        assert not js("return window.__errors"), js("return window.__errors")
        metrics["continuity"] = {
            "stable_rows": True,
            "empty_frames": js("return window.__emptyFrames"),
            "composed_source": source,
            "remix_matches": None,
        }
        js(
            "window.__legacyQuestion={...__probe.questionDocument(),..."
            + json.dumps(
                {
                    "corpus": state["dataset"],
                    "analysis": state["contract_run"]["analysis_identity"],
                }
            )
            + ",format:'lingrep.question-probe/v1',question:'How is the old title used?',source:'legacy := か も しれ ない.',metadata:'{}',raw:'',where:'',context:8,evidence:'',sections:'[]',attempt:'',view:''};window.open=url=>{window.__openedQuestion=url;return null};const transfer=new DataTransfer();transfer.items.add(new File([JSON.stringify(__legacyQuestion)],'question.json',{type:'application/json'}));const input=document.querySelector('#open-question');input.files=transfer.files;input.dispatchEvent(new Event('change'))"
        )
        imported = wait("return window.__openedQuestion")
        assert js("return !new URL(__openedQuestion).hash.includes('question=')")
        req(prefix + "/url", {"url": imported})
        req(prefix + "/refresh", {})
        wait("return window.__probe?.state.current?.phase==='complete'")
        assert (
            js("return document.querySelector('#source').value")
            == "legacy := か も しれ ない."
        )
        assert js("return document.querySelector('#context-size').value") == "8"
        assert js("return __probe.state.current.received") == args.expected_matches
        assert js("return !document.querySelector('#question')")
        before_edit = js("return __probe.state.current.id")
        js(
            "window.__submitRequests=[];window.__submitFetch=window.fetch;window.fetch=(url,options)=>{if(new URL(String(url),document.baseURI).pathname.endsWith('/api/questions/runs')&&options?.method==='POST')__submitRequests.push(options.body);return __submitFetch(url,options)};document.querySelector('#edit-source').click();document.querySelector('#source').value='かもしれない := か も しれ ない が.';document.querySelector('#source').dispatchEvent(new Event('input'))"
        )
        source_element = req(
            prefix + "/element", {"using": "css selector", "value": "#source"}
        )["element-6066-11e4-a52e-4f735466cecf"]
        req(
            prefix + "/element/" + source_element + "/value",
            {"text": "\ue00a\ue007\ue000"},
        )
        changed = wait(
            "return __probe.state.current?.id!=="
            + json.dumps(before_edit)
            + " && __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        assert changed["source"] == "かもしれない := か も しれ ない が."
        assert changed["received"] == 0
        wait(
            "return document.querySelector('.result-panel')?.dataset.run==="
            + json.dumps(changed["id"])
            + " && document.querySelectorAll('.kwic-row').length===0"
        )
        js("document.querySelector('#source').blur()")
        assert js("return __submitRequests.length") == 1
        js(
            "document.querySelector('#source').focus();document.querySelector('#source').value='かもしれない := か も しれ ない.';document.querySelector('#source').dispatchEvent(new Event('input'));document.querySelector('#source').blur()"
        )
        assert js("return __submitRequests.length") == 1
        assert js("return !document.querySelector('#draft-badge').hidden")
        js("document.querySelector('#run').click()")
        restored = wait(
            "return __probe.state.current?.id!=="
            + json.dumps(changed["id"])
            + " && __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        assert restored["received"] == args.expected_matches
        wait("return document.querySelectorAll('.kwic-row').length===20")
        assert js("return __submitRequests.length") == 2
        js(
            "document.querySelector('#source').focus();document.querySelector('#source').blur()"
        )
        assert js("return __submitRequests.length") == 2
        js(
            "window.fetch=window.__submitFetch;document.querySelector('.kwic-row').click()"
        )
        wait("return document.querySelector('#token-ribbon .selected')")
        js("document.querySelector('#token-attributes>details').open=true")
        for width, expected_attributes in [(1440, 18), (850, 12), (390, 6)]:
            req(prefix + "/window/rect", {"width": width, "height": 1100})
            wait(
                "return [...document.querySelectorAll('.attribute-basics .attribute-row')].filter(row=>row.getBoundingClientRect().height>0).length==="
                + str(expected_attributes)
            )
        metrics["editor"] = {
            "alt_enter": True,
            "blur": True,
            "duplicate_submissions": 0,
        }
        js(
            "document.querySelector('#clear-pattern').click();document.querySelector('[data-attribute=\"lemma\"]').click();document.querySelector('#attribute-remix').click();window.__sectionParent=document.querySelector('.result-panel');window.__sectionRow=document.querySelector('.kwic-row')"
        )
        derived_source = js(
            "return document.querySelector('#pattern-preview').textContent"
        )
        parent_id = js("return __probe.state.current.id")
        original_handles = req(prefix + "/window/handles")
        js(
            "window.__deriveFetch=window.fetch;window.fetch=(url,options)=>new URL(String(url),document.baseURI).pathname.endsWith('/api/questions/runs')&&options?.method==='POST'?Promise.reject(new Error('Search connection interrupted')):__deriveFetch(url,options);document.querySelector('#use-pattern').click()"
        )
        wait("return __probe.state.current?.phase==='failed'")
        assert js(
            "return document.querySelector('.result-panel')===__sectionParent && document.querySelector('.kwic-row')===__sectionRow"
        )
        js("window.fetch=__deriveFetch;document.querySelector('#retry').click()")
        remixed = wait(
            "return __probe.state.current?.id!=="
            + json.dumps(parent_id)
            + " && __probe.state.current?.phase==='complete' && __probe.state.current"
        )
        assert remixed["source"] == derived_source
        assert req(prefix + "/window/handles") == original_handles
        wait(
            "return document.querySelectorAll('.result-panel').length===2 && document.querySelectorAll('.result-panel')[1].querySelector('.kwic-row')"
        )
        assert js(
            "return document.querySelector('.result-panel')===__sectionParent && document.querySelector('.kwic-row')===__sectionRow"
        )
        assert parent_id in js(
            "return document.querySelector('.section-origin a').getAttribute('href')"
        )
        assert len(json.loads(js("return __probe.questionDocument().sections"))) == 2
        assert js("return __probe.questionDocument().attempt") == remixed["id"]
        metrics["continuity"]["remix_matches"] = remixed["received"]
        capture("linked-sections", 1440, 1100)
        js("document.querySelector('.section-origin a').click()")
        wait("return __probe.view.kind==='example'")
        req(prefix + "/refresh", {})
        wait(
            "return !!window.__probe && document.querySelectorAll('.result-panel').length===2"
        )
        assert js("return __probe.state.current.id") == remixed["id"]
        assert js("return [...document.querySelectorAll('.result-panel')].map(panel=>panel.dataset.run)") == [parent_id, remixed["id"]]
        saved_document = js("return __probe.questionDocument()")
        original = req(prefix + "/window")
        js("document.querySelector('#remix').click()")
        deadline = time.monotonic() + 10
        while len(handles := req(prefix + "/window/handles")) < 2:
            assert time.monotonic() < deadline
            time.sleep(0.1)
        new = next(handle for handle in handles if handle != original)
        req(prefix + "/window", {"handle": new})
        wait(
            "return window.__probe && document.querySelectorAll('.result-panel').length===2"
        )
        assert js("return __probe.state.current.id") == remixed["id"]
        req(prefix + "/window", method="DELETE")
        req(prefix + "/window", {"handle": original})
        req(prefix + "/window/rect", {"width": 390, "height": 844})
        req(
            prefix + "/url",
            {
                "url": args.url.split("#")[0]
                + "?check=focused-example#evidence="
                + parent_id
                + "&fresh=1"
            },
        )
        wait(
            "return window.__probe && document.querySelectorAll('.kwic-row').length===20"
        )
        assert js("return document.querySelector('#reading-options').open")
        js("document.querySelector('.kwic-row').click()")
        wait(
            "return document.querySelector('#focused-sentence').getBoundingClientRect().height>0"
        )
        assert not js("return document.querySelector('#token-details').open")
        assert js(
            "return document.querySelector('#focused-sentence').textContent"
        ) == js(
            "return [...document.querySelectorAll('#token-ribbon button')].map(button=>button.textContent).join('')"
        )
        sentence = js("return document.querySelector('#focused-sentence').textContent")
        js(
            "document.querySelector('#token-lab').scrollIntoView({block:'start'});window.__focusedTop=document.querySelector('#token-lab').getBoundingClientRect().top;document.querySelector('[data-example-step=\"1\"]').focus({preventScroll:true})"
        )
        step_element = req(
            prefix + "/element",
            {"using": "css selector", "value": '[data-example-step="1"]'},
        )["element-6066-11e4-a52e-4f735466cecf"]
        req(prefix + "/element/" + step_element + "/value", {"text": "\ue007"})
        wait(
            "return document.querySelector('#focused-sentence').textContent!=="
            + json.dumps(sentence)
        )
        assert (
            js(
                "return Math.abs(document.querySelector('#token-lab').getBoundingClientRect().top-__focusedTop)"
            )
            <= 1
        )
        assert not js("return document.querySelector('#token-details').open")
        assert js("return document.activeElement?.dataset.exampleStep") == "1"
        assert js(
            "return getComputedStyle(document.querySelector('#focused-sentence mark')).backgroundColor"
        ) == js(
            "return getComputedStyle(document.querySelector('.kwic-row mark')).backgroundColor"
        )
        capture("readable-example", 390, 844)
        expired_document = dict(saved_document)
        links = json.loads(expired_document["sections"])
        expired_ids = {link["id"]: format(index + 1000000, "032x") for index, link in enumerate(links)}
        for link in links:
            link["id"] = expired_ids[link["id"]]
            if link["origin"]:
                link["origin"]["parent"] = expired_ids[link["origin"]["parent"]]
        expired_document.update({"sections": json.dumps(links, ensure_ascii=False), "evidence": links[-1]["id"], "attempt": links[-1]["id"], "fresh": "1", "run": "1"})
        req(prefix + "/url", {"url": query_base + "?check=expired#" + urllib.parse.urlencode(expired_document)})
        wait("return window.__probe && !document.querySelector('#saved-searches').hidden")
        assert js("return __probe.state.current") is None
        assert js("return JSON.parse(__probe.questionDocument().sections)") == links
        js("document.querySelector('#restore-searches').click()")
        wait("return __probe.state.current?.phase==='complete' && document.querySelector('#saved-searches').hidden && document.querySelectorAll('.result-panel').length===2", seconds=120)
        restored_links = json.loads(js("return __probe.questionDocument().sections"))
        assert [link["recipe"] for link in restored_links] == [link["recipe"] for link in links]
        assert not any(link["id"] in expired_ids.values() for link in restored_links)
        assert restored_links[1]["origin"]["parent"] == restored_links[0]["id"]
        metrics["saved-recipes"] = {"expired_requires_explicit_action": True, "lineage_restored": True}
        js("window.__retainedNoticeRow=document.querySelector('.kwic-row');document.querySelector('#source').value='unavailable := [dep.rel=nsubj].';document.querySelector('#source').dispatchEvent(new Event('input'));__probe.search()")
        wait("return __probe.state.current?.phase==='complete' && __probe.state.current.source==='unavailable := [dep.rel=nsubj].'")
        wait("return [...document.querySelectorAll('.result-notices:not([hidden])')].some(list=>list.textContent.includes('dep.rel'))")
        assert js("return __retainedNoticeRow.isConnected && document.querySelector('.kwic-row')===__retainedNoticeRow")
        metrics["notices"] = {"unavailable_attribute_visible": True, "retained_evidence_preserved": True}

        metrics["linked-sections"] = {
            "same_document": True,
            "restored_after_reload": True,
            "copied_with_lineage": True,
        }
        long_name = "pattern_" + "a" * 100
        js("document.querySelector('#source').value=" + json.dumps(long_name + " := か も しれ ない.") + ";document.querySelector('#source').dispatchEvent(new Event('input'));__probe.search()")
        wait("return __probe.state.current?.phase==='complete' && document.querySelector('#source-label').textContent===" + json.dumps(long_name))
        metrics["long-pattern"] = capture("long-pattern", 320, 844)
        assert js("return document.querySelector('#edit-source').getBoundingClientRect().width<=document.querySelector('main').getBoundingClientRect().width")
        metrics["accessibility"] = {"file_focus_visible":True,"close_returns_focus":True,"committed_numeric_errors":True,"context_nodes_retained":True}
        (out / "browser-metrics.json").write_text(
            json.dumps(metrics, ensure_ascii=False, indent=2)
        )
        print(
            json.dumps(
                {
                    "first_run": first_id,
                    "matches": state["received"],
                    "counts": state["counts"],
                    "stopped": stopped["id"],
                    "checks": list(metrics),
                },
                ensure_ascii=False,
            ),
            flush=True,
        )
    finally:
        if session:
            try:
                req("/session/" + session, method="DELETE")
            except (urllib.error.URLError, OSError) as error:
                print(f"Could not close the browser session: {error}", file=sys.stderr)
        driver.terminate()
        driver.wait(timeout=15)

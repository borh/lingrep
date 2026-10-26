import assert from "node:assert/strict";
import test from "node:test";
import { defaultScope, readScope } from "../assets/search-scope.js";

const preferred = ["introduction", "methods", "results", "discussion"];
test("IMRAD default uses only labels the destination corpus carries", () => {
    assert.deepEqual(defaultScope(["abstract", "Introduction", "methods", "title"], preferred), {
        included_sections: ["Introduction", "methods"], include_unlabelled: false,
    });
    for (const labels of [[], ["news", "fiction"]]) {
        assert.deepEqual(defaultScope(labels, preferred), { included_sections: null, include_unlabelled: true });
    }
});

test("unchecked sections mean no labelled text, rather than all text", () => {
    const inputs = [{ value: "abstract", checked: false }, { value: "methods", checked: false }];
    const control = {
        querySelectorAll: () => inputs,
        querySelector: () => ({ checked: false }),
    };
    assert.deepEqual(readScope(control), { included_sections: [], include_unlabelled: false });
    inputs[1].checked = true;
    assert.deepEqual(readScope(control), { included_sections: ["methods"], include_unlabelled: false });
    inputs[0].checked = true;
    assert.deepEqual(readScope(control), { included_sections: null, include_unlabelled: false });
});

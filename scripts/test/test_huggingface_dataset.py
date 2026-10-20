import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "huggingface_dataset", ROOT / "scripts" / "huggingface_dataset.py"
)
HF = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(HF)


class HuggingFaceDatasetAdapterTests(unittest.TestCase):
    @staticmethod
    def ud_row():
        return {
            "sent_id": "test-1",
            "text": "猫が寝る。",
            "comments": ["__SENT_ID__", "__TEXT__"],
            "tokens": ["猫", "が", "寝る", "。"],
            "lemmas": ["猫", "が", "寝る", "。"],
            "upos": ["NOUN", "ADP", "VERB", "PUNCT"],
            "xpos": ["名詞", "助詞", "動詞", "補助記号"],
            "feats": ["_", "_", "_", "_"],
            "head": [3, 1, 0, 3],
            "deprel": ["nsubj", "case", "root", "punct"],
            "deps": ["3:nsubj", "1:case", "0:root", "3:punct"],
            "misc": ["_", "_", "_", "_"],
            "mwt": [],
            "empty_nodes": [],
        }

    def test_inspection_lists_configs_and_focuses_one_schema_without_rows(self):
        class Features:
            @staticmethod
            def to_dict():
                return {
                    "translation": {
                        "_type": "Translation",
                        "languages": ["en", "ja"],
                    }
                }

        class Info:
            features = Features()

        class Builder:
            info = Info()

        arguments = HF.parser().parse_args(
            ["owner/dataset", "--inspect", "--config", "en-ja"]
        )
        record = HF.inspect_dataset(
            arguments,
            config_names=lambda *_args, **_kwargs: ["de-ja", "en-ja"],
            split_names=lambda *_args, **_kwargs: ["train", "test"],
            load_builder=lambda *_args, **_kwargs: Builder(),
        )

        self.assertEqual(record["configurations"], ["de-ja", "en-ja"])
        self.assertEqual(record["selected_configuration"], "en-ja")
        self.assertEqual(record["splits"], ["train", "test"])
        self.assertEqual(record["features"]["translation"]["languages"], ["en", "ja"])

    def test_inspection_of_many_configs_requests_a_focus_without_loading_one(self):
        arguments = HF.parser().parse_args(["owner/dataset", "--inspect"])

        record = HF.inspect_dataset(
            arguments,
            config_names=lambda *_args, **_kwargs: ["a", "b"],
            split_names=lambda *_args, **_kwargs: self.fail(
                "split read was not bounded"
            ),
            load_builder=lambda *_args, **_kwargs: self.fail("builder was loaded"),
        )

        self.assertIsNone(record["selected_configuration"])
        self.assertIn("--config", record["hint"])

    def test_rows_stream_as_text_documents_with_selected_nested_metadata(self):
        rows = [
            {"translation": {"ja": "猫。犬。", "en": "Cat. Dog."}, "id": 7},
            {"translation": {"ja": "鳥。", "en": "Bird."}, "id": 8},
        ]
        output = io.StringIO()

        count = HF.write_rows(
            rows,
            output,
            text_field="translation.ja",
            metadata_fields=["id", "translation.en"],
            limit=None,
        )

        self.assertEqual(count, 2)
        self.assertEqual(
            [json.loads(line) for line in output.getvalue().splitlines()],
            [
                {
                    "text": "猫。犬。",
                    "metadata": {"id": 7, "translation.en": "Cat. Dog."},
                },
                {
                    "text": "鳥。",
                    "metadata": {"id": 8, "translation.en": "Bird."},
                },
            ],
        )

    def test_limit_stops_before_requesting_another_streamed_row(self):
        def rows():
            yield {"text": "一。"}
            raise AssertionError("adapter read beyond the requested limit")

        output = io.StringIO()
        count = HF.write_rows(
            rows(),
            output,
            text_field="text",
            metadata_fields=[],
            limit=1,
        )

        self.assertEqual(count, 1)
        self.assertEqual(json.loads(output.getvalue()), {"text": "一。"})

    def test_conllu_rows_are_schema_checked_streamed_and_limited(self):
        row = self.ud_row()

        def rows():
            yield row
            raise AssertionError("adapter read beyond the requested limit")

        def encode(example, upos_names):
            return "# sent_id = test-1\n1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t_\t_\n\n"

        output = io.StringIO()
        with mock.patch.object(HF, "encode_conllu_row", side_effect=encode) as encoder:
            count = HF.write_conllu_rows(rows(), output, limit=1, upos_names=None)

        self.assertEqual(count, 1)
        encoder.assert_called_once_with(row, None)
        self.assertTrue(output.getvalue().startswith("# sent_id = test-1"))

    def test_conllu_schema_errors_name_the_row_and_invalid_column(self):
        missing = self.ud_row()
        del missing["head"]
        mismatched = self.ud_row()
        mismatched["deprel"] = ["root"]

        for row, message in [
            (missing, "row 3: CoNLL-U field 'head' is missing"),
            (
                mismatched,
                "row 3: CoNLL-U field 'deprel' has 1 values; expected 4",
            ),
        ]:
            with (
                self.subTest(message=message),
                self.assertRaisesRegex(HF.ProjectionError, message),
            ):
                HF.validate_conllu_row(row, row_number=3)

    def test_conllu_mode_rejects_jsonl_projection_options(self):
        for option in ("--text-field", "--metadata-field"):
            arguments = HF.parser().parse_args(
                [
                    "owner/dataset",
                    "--split",
                    "train",
                    "--output-format",
                    "conllu",
                    option,
                    "text",
                ]
            )
            with (
                self.subTest(option=option),
                self.assertRaisesRegex(
                    HF.ProjectionError, f"{option} is only valid with JSONL output"
                ),
            ):
                HF.validate_projection_options(arguments)

    def test_selected_output_format_dispatches_to_conllu_projection(self):
        arguments = HF.parser().parse_args(
            [
                "owner/dataset",
                "--split",
                "train",
                "--output-format",
                "conllu",
                "--limit",
                "2",
            ]
        )
        rows = [self.ud_row()]

        with mock.patch.object(HF, "write_conllu_rows", return_value=1) as writer:
            count = HF.write_selected_rows(rows, io.StringIO(), arguments)

        self.assertEqual(count, 1)
        writer.assert_called_once()
        self.assertIs(writer.call_args.args[0], rows)
        self.assertEqual(writer.call_args.kwargs["limit"], 2)

    def test_materialization_is_atomic_and_refuses_replacement(self):
        arguments = HF.parser().parse_args(
            ["owner/dataset", "--split", "train", "--output", "unused.jsonl"]
        )
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "corpus.jsonl"
            count = HF.write_new_file(destination, [{"text": "猫。"}], arguments)
            self.assertEqual(count, 1)
            self.assertEqual(json.loads(destination.read_text()), {"text": "猫。"})

            with self.assertRaisesRegex(FileExistsError, "already exists"):
                HF.write_new_file(destination, [{"text": "犬。"}], arguments)
            self.assertEqual(json.loads(destination.read_text()), {"text": "猫。"})

            failed_destination = Path(directory) / "failed.jsonl"
            with self.assertRaises(HF.ProjectionError):
                HF.write_new_file(failed_destination, [{}], arguments)
            self.assertFalse(failed_destination.exists())
            self.assertEqual(list(Path(directory).glob("*.partial")), [])

    def test_missing_or_non_string_text_names_the_row_and_field(self):
        for row, message in [
            ({}, "row 3: field 'text' is missing"),
            ({"text": ["猫。"]}, "row 3: text field 'text' must be a string"),
        ]:
            with (
                self.subTest(row=row),
                self.assertRaisesRegex(HF.ProjectionError, message),
            ):
                HF.project_row(
                    row,
                    row_number=3,
                    text_field="text",
                    metadata_fields=[],
                )


if __name__ == "__main__":
    unittest.main()

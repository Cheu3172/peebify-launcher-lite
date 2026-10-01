import tsParser from "@typescript-eslint/parser";
import reactHooks from "eslint-plugin-react-hooks";

export default [
  { ignores: ["node_modules/**", "dist/**"] },
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsParser,
      parserOptions: { ecmaVersion: 2022, sourceType: "module", ecmaFeatures: { jsx: true } },
    },
    plugins: {
      "react-hooks": reactHooks,
    },
    rules: {
      "no-console": "error",
      "no-empty": ["error", { allowEmptyCatch: true }],
      "no-restricted-syntax": [
        "error",
        {
          selector:
            "CallExpression[callee.property.name='catch'] > ArrowFunctionExpression[body.type='BlockStatement'][body.body.length=0]",
          message:
            "Empty .catch(() => {}) swallows errors. Surface failures (rpcAction/notify) or log them.",
        },
        {
          selector:
            "CallExpression[callee.property.name='catch'] > FunctionExpression[body.body.length=0]",
          message:
            "Empty .catch(function () {}) swallows errors. Surface failures (rpcAction/notify) or log them.",
        },
      ],
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
  {
    files: ["src/lib/log.ts"],
    rules: { "no-console": "off" },
  },
  {
    files: ["src/components/ui/Select.tsx"],
    rules: { "react-hooks/exhaustive-deps": "off" },
  },
];

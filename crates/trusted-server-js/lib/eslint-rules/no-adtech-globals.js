const ADTECH_GLOBALS = new Set(['googletag', 'pbjs']);
const GLOBAL_ROOTS = new Set(['globalThis', 'self', 'window']);

// Known blind spots include computed composition (`globalThis['goog' + 'letag']`)
// and function-returned roots (`getWin().googletag`); adapter boundaries and
// restricted imports remain defense in depth.

export const LEGACY_ADTECH_GLOBAL_ALLOWLIST = Object.freeze([
  'src/integrations/gpt/index.ts',
  'src/integrations/gpt_diagnostics/observer.ts',
  'src/integrations/prebid/index.ts',
]);

export const LEGACY_RESTRICTED_IMPORT_ALLOWLIST = Object.freeze([
  'src/core/request.ts',
  'src/integrations/gpt/index.ts',
  'src/integrations/prebid/index.ts',
]);

function normalizeFilename(filename, rootDirectory) {
  const normalized = filename.replaceAll('\\', '/');
  if (!rootDirectory) return normalized.startsWith('./') ? normalized.slice(2) : normalized;

  const normalizedRoot = rootDirectory.replaceAll('\\', '/').replace(/\/$/, '');
  const rootPrefix = `${normalizedRoot}/`;
  return normalized.startsWith(rootPrefix) ? normalized.slice(rootPrefix.length) : normalized;
}

function staticPropertyName(node) {
  if (!node.computed && node.property.type === 'Identifier') {
    return node.property.name;
  }
  if (!node.computed && node.property.type === 'PrivateIdentifier') {
    return `#${node.property.name}`;
  }
  if (
    node.computed &&
    node.property.type === 'Literal' &&
    typeof node.property.value === 'string'
  ) {
    return node.property.value;
  }
  if (
    node.computed &&
    node.property.type === 'TemplateLiteral' &&
    node.property.expressions.length === 0
  ) {
    return node.property.quasis[0]?.value.cooked;
  }
  return undefined;
}

function staticPatternPropertyName(property) {
  if (!property.computed && property.key.type === 'Identifier') return property.key.name;
  if (property.key.type === 'Literal' && typeof property.key.value === 'string') {
    return property.key.value;
  }
  if (
    property.computed &&
    property.key.type === 'TemplateLiteral' &&
    property.key.expressions.length === 0
  ) {
    return property.key.quasis[0]?.value.cooked;
  }
  return undefined;
}

function staticClassElementName(element) {
  if (!element.computed && element.key.type === 'Identifier') return element.key.name;
  if (!element.computed && element.key.type === 'PrivateIdentifier') {
    return `#${element.key.name}`;
  }
  if (element.key.type === 'Literal' && typeof element.key.value === 'string') {
    return element.key.value;
  }
  return undefined;
}

function unwrapExpression(node) {
  let current = node;
  while (
    current &&
    [
      'ChainExpression',
      'TSAsExpression',
      'TSInstantiationExpression',
      'TSNonNullExpression',
      'TSTypeAssertion',
    ].includes(current.type)
  ) {
    current = current.expression;
  }
  return current;
}

function strongerOrigin(left, right) {
  if (left === 'adtech' || right === 'adtech') return 'adtech';
  if (left === 'root' || right === 'root') return 'root';
  return 'unknown';
}

export default {
  meta: {
    type: 'problem',
    docs: {
      description: 'Keep GPT and Prebid globals behind TSJS adapter interfaces.',
    },
    schema: [
      {
        type: 'object',
        properties: {
          allowFiles: {
            type: 'array',
            items: { type: 'string' },
            uniqueItems: true,
          },
          rootDirectory: { type: 'string' },
        },
        additionalProperties: false,
      },
    ],
    messages: {
      externalGlobalOwnedByAdapter:
        'Access to "{{name}}" is owned by src/adapters; inject an adapter interface instead.',
    },
  },

  create(context) {
    const sourceCode = context.sourceCode;
    const relativeFilename = normalizeFilename(context.filename, context.options[0]?.rootDirectory);
    const allowFiles = new Set(context.options[0]?.allowFiles ?? []);
    const isAdapter = relativeFilename.startsWith('src/adapters/');

    if (isAdapter || allowFiles.has(relativeFilename)) return {};

    const assignments = new Map();
    const patternAssignments = new Map();
    const loopAssignments = new Map();
    const loopPatternAssignments = new Map();
    const thisPropertyAssignments = new Map();
    const classOwnerTokens = new WeakMap();
    const candidateMembers = [];
    const candidateIdentifiers = [];
    const candidatePatterns = [];
    const reported = new Set();

    function classOwnerToken(classNode, isStatic) {
      let tokens = classOwnerTokens.get(classNode);
      if (!tokens) {
        tokens = { instance: {}, static: {} };
        classOwnerTokens.set(classNode, tokens);
      }
      return isStatic ? tokens.static : tokens.instance;
    }

    function thisOwner(thisExpression) {
      let current = thisExpression.parent;
      let staticClassContext = false;

      while (current) {
        if (current.type === 'MethodDefinition' || current.type === 'PropertyDefinition') {
          staticClassContext = current.static;
        } else if (current.type === 'StaticBlock') {
          staticClassContext = true;
        } else if (current.type === 'ClassDeclaration' || current.type === 'ClassExpression') {
          return classOwnerToken(current, staticClassContext);
        } else if (
          current.type === 'FunctionDeclaration' ||
          current.type === 'FunctionExpression'
        ) {
          const parent = current.parent;
          if (parent?.type === 'MethodDefinition') {
            current = parent;
            continue;
          }
          if (
            parent?.type === 'Property' &&
            parent.method &&
            parent.parent?.type === 'ObjectExpression'
          ) {
            return parent.parent;
          }
          return current;
        }
        current = current.parent;
      }

      return sourceCode.ast;
    }

    function thisPropertyEntry(owner, propertyName, create) {
      let properties = thisPropertyAssignments.get(owner);
      if (!properties && create) {
        properties = new Map();
        thisPropertyAssignments.set(owner, properties);
      }
      if (!properties) return undefined;

      let entry = properties.get(propertyName);
      if (!entry && create) {
        entry = { expressions: [] };
        properties.set(propertyName, entry);
      }
      return entry;
    }

    function recordThisProperty(owner, propertyName, expression) {
      thisPropertyEntry(owner, propertyName, true).expressions.push(expression);
    }

    function findVariable(identifier) {
      let scope = sourceCode.getScope(identifier);
      while (scope) {
        const variable = scope.set.get(identifier.name);
        if (variable) return variable;
        scope = scope.upper;
      }
      return undefined;
    }

    function isUnshadowedGlobal(identifier, names) {
      if (!names.has(identifier.name)) return false;
      const variable = findVariable(identifier);
      return !variable || variable.defs.length === 0;
    }

    function isReference(identifier) {
      let scope = sourceCode.getScope(identifier);
      while (scope) {
        if (scope.references.some((reference) => reference.identifier === identifier)) return true;
        scope = scope.upper;
      }
      return false;
    }

    function patternOriginFromBase(pattern, initializerOrigin, variableName) {
      if (pattern.type !== 'ObjectPattern') return 'unknown';
      if (initializerOrigin !== 'root' && initializerOrigin !== 'adtech') return 'unknown';

      for (const property of pattern.properties) {
        if (property.type === 'RestElement') {
          if (property.argument.type === 'Identifier' && property.argument.name === variableName) {
            return initializerOrigin;
          }
          continue;
        }
        const value =
          property.value.type === 'AssignmentPattern' ? property.value.left : property.value;
        if (value.type !== 'Identifier' || value.name !== variableName) continue;

        const propertyName = staticPatternPropertyName(property);

        if (initializerOrigin === 'root' && ADTECH_GLOBALS.has(propertyName)) return 'adtech';
        if (initializerOrigin === 'root' && propertyName === 'window') return 'root';
        return initializerOrigin === 'adtech' ? 'adtech' : 'unknown';
      }
      return 'unknown';
    }

    function patternOrigin(pattern, initializer, variableName, seen) {
      return patternOriginFromBase(pattern, expressionOrigin(initializer, seen), variableName);
    }

    function variableOrigin(variable, seen) {
      if (seen.has(variable)) return 'unknown';
      const nextSeen = new Set(seen).add(variable);
      let result = 'unknown';

      for (const definition of variable.defs) {
        if (definition.type === 'Variable') {
          const declaration = definition.node;
          if (!declaration.init) continue;

          if (declaration.id.type === 'Identifier') {
            result = strongerOrigin(result, expressionOrigin(declaration.init, nextSeen));
          } else {
            result = strongerOrigin(
              result,
              patternOrigin(declaration.id, declaration.init, variable.name, nextSeen)
            );
          }
        } else if (definition.type === 'Parameter') {
          let parameter = definition.node.params?.[definition.index];
          if (parameter?.type === 'TSParameterProperty') parameter = parameter.parameter;
          if (parameter?.type !== 'AssignmentPattern') continue;

          if (parameter.left.type === 'Identifier') {
            result = strongerOrigin(result, expressionOrigin(parameter.right, nextSeen));
          } else {
            result = strongerOrigin(
              result,
              patternOrigin(parameter.left, parameter.right, variable.name, nextSeen)
            );
          }
        }
      }

      for (const expression of assignments.get(variable) ?? []) {
        result = strongerOrigin(result, expressionOrigin(expression, nextSeen));
      }
      for (const { pattern, initializer } of patternAssignments.get(variable) ?? []) {
        result = strongerOrigin(
          result,
          patternOrigin(pattern, initializer, variable.name, nextSeen)
        );
      }
      for (const iterable of loopAssignments.get(variable) ?? []) {
        result = strongerOrigin(result, iterableElementOrigin(iterable, nextSeen));
      }
      for (const { pattern, iterable } of loopPatternAssignments.get(variable) ?? []) {
        result = strongerOrigin(
          result,
          patternOriginFromBase(pattern, iterableElementOrigin(iterable, nextSeen), variable.name)
        );
      }
      return result;
    }

    function iterableElementOrigin(rawNode, seen = new Set()) {
      const node = unwrapExpression(rawNode);
      if (!node) return 'unknown';

      if (node.type === 'ArrayExpression') {
        return node.elements.reduce((result, element) => {
          if (!element) return result;
          const origin =
            element.type === 'SpreadElement'
              ? iterableElementOrigin(element.argument, seen)
              : expressionOrigin(element, seen);
          return strongerOrigin(result, origin);
        }, 'unknown');
      }

      if (node.type === 'Identifier') {
        const variable = findVariable(node);
        if (!variable || seen.has(variable)) return 'unknown';
        const nextSeen = new Set(seen).add(variable);
        let result = 'unknown';
        for (const definition of variable.defs) {
          if (definition.type !== 'Variable' || !definition.node.init) continue;
          result = strongerOrigin(result, iterableElementOrigin(definition.node.init, nextSeen));
        }
        for (const expression of assignments.get(variable) ?? []) {
          result = strongerOrigin(result, iterableElementOrigin(expression, nextSeen));
        }
        return result;
      }

      if (node.type === 'SequenceExpression') {
        return iterableElementOrigin(node.expressions.at(-1), seen);
      }
      if (node.type === 'LogicalExpression' || node.type === 'ConditionalExpression') {
        const branches =
          node.type === 'ConditionalExpression'
            ? [node.consequent, node.alternate]
            : [node.left, node.right];
        return branches.reduce(
          (result, branch) => strongerOrigin(result, iterableElementOrigin(branch, seen)),
          'unknown'
        );
      }
      return 'unknown';
    }

    function expressionOrigin(rawNode, seen = new Set()) {
      const node = unwrapExpression(rawNode);
      if (!node) return 'unknown';

      if (node.type === 'Identifier') {
        if (isUnshadowedGlobal(node, GLOBAL_ROOTS)) return 'root';
        if (isUnshadowedGlobal(node, ADTECH_GLOBALS)) return 'adtech';
        const variable = findVariable(node);
        return variable ? variableOrigin(variable, seen) : 'unknown';
      }

      if (node.type === 'MemberExpression') {
        const propertyName = staticPropertyName(node);
        if (node.object.type === 'ThisExpression') {
          const entry = thisPropertyEntry(thisOwner(node.object), propertyName, false);
          if (!entry || seen.has(entry)) return 'unknown';
          const nextSeen = new Set(seen).add(entry);
          return entry.expressions.reduce(
            (result, expression) => strongerOrigin(result, expressionOrigin(expression, nextSeen)),
            'unknown'
          );
        }

        const objectOrigin = expressionOrigin(node.object, seen);
        if (objectOrigin === 'root' && ADTECH_GLOBALS.has(propertyName)) return 'adtech';
        if (objectOrigin === 'root' && propertyName === 'window') return 'root';
        if (objectOrigin === 'adtech') return 'adtech';
        return 'unknown';
      }

      if (node.type === 'AssignmentExpression') return expressionOrigin(node.right, seen);
      if (node.type === 'SequenceExpression') {
        return expressionOrigin(node.expressions.at(-1), seen);
      }
      if (node.type === 'LogicalExpression' || node.type === 'ConditionalExpression') {
        const branches =
          node.type === 'ConditionalExpression'
            ? [node.consequent, node.alternate]
            : [node.left, node.right];
        return branches.reduce(
          (result, branch) => strongerOrigin(result, expressionOrigin(branch, seen)),
          'unknown'
        );
      }
      return 'unknown';
    }

    function report(node, name) {
      const key = `${node.range?.[0] ?? node.loc.start.line}:${node.range?.[1] ?? node.loc.end.column}`;
      if (reported.has(key)) return;
      reported.add(key);
      context.report({
        node,
        messageId: 'externalGlobalOwnedByAdapter',
        data: { name },
      });
    }

    function recordPatternAssignments(pattern, initializer) {
      for (const property of pattern.properties) {
        const value = property.type === 'RestElement' ? property.argument : property.value;
        const target = value.type === 'AssignmentPattern' ? value.left : value;
        if (target.type !== 'Identifier') continue;
        const variable = findVariable(target);
        if (!variable) continue;
        const entries = patternAssignments.get(variable) ?? [];
        entries.push({ pattern, initializer });
        patternAssignments.set(variable, entries);
      }
    }

    function recordVariableAssignment(identifier, expression) {
      const variable = findVariable(identifier);
      if (!variable) return;
      const values = assignments.get(variable) ?? [];
      values.push(expression);
      assignments.set(variable, values);
    }

    function recordLoopBinding(rawBinding, iterable) {
      const binding = rawBinding.type === 'AssignmentPattern' ? rawBinding.left : rawBinding;
      if (binding.type === 'Identifier') {
        const variable = findVariable(binding);
        if (!variable) return;
        const values = loopAssignments.get(variable) ?? [];
        values.push(iterable);
        loopAssignments.set(variable, values);
      } else if (binding.type === 'ObjectPattern') {
        candidatePatterns.push({ pattern: binding, initializer: iterable, iterable: true });
        for (const property of binding.properties) {
          const value = property.type === 'RestElement' ? property.argument : property.value;
          const target = value.type === 'AssignmentPattern' ? value.left : value;
          if (target.type !== 'Identifier') continue;
          const variable = findVariable(target);
          if (!variable) continue;
          const entries = loopPatternAssignments.get(variable) ?? [];
          entries.push({ pattern: binding, iterable });
          loopPatternAssignments.set(variable, entries);
        }
      }
    }

    return {
      AssignmentExpression(node) {
        const left = unwrapExpression(node.left);
        if (left.type === 'ObjectPattern') {
          candidatePatterns.push({ pattern: left, initializer: node.right });
          recordPatternAssignments(left, node.right);
          return;
        }
        if (left.type === 'MemberExpression' && left.object.type === 'ThisExpression') {
          const propertyName = staticPropertyName(left);
          if (!propertyName) return;
          recordThisProperty(thisOwner(left.object), propertyName, node.right);
          return;
        }
        if (left.type !== 'Identifier') return;
        recordVariableAssignment(left, node.right);
      },

      ForOfStatement(node) {
        if (node.left.type === 'VariableDeclaration') {
          for (const declaration of node.left.declarations) {
            recordLoopBinding(declaration.id, node.right);
          }
        } else {
          recordLoopBinding(node.left, node.right);
        }
      },

      MemberExpression(node) {
        candidateMembers.push(node);
      },

      Identifier(node) {
        candidateIdentifiers.push(node);
      },

      PropertyDefinition(node) {
        if (!node.value) return;
        const propertyName = staticClassElementName(node);
        const classNode = node.parent?.parent;
        if (
          !propertyName ||
          (classNode?.type !== 'ClassDeclaration' && classNode?.type !== 'ClassExpression')
        ) {
          return;
        }
        recordThisProperty(classOwnerToken(classNode, node.static), propertyName, node.value);
      },

      VariableDeclarator(node) {
        if (node.id.type === 'ObjectPattern' && node.init) {
          candidatePatterns.push({ pattern: node.id, initializer: node.init });
        }
      },

      'Program:exit'() {
        for (const { pattern, initializer, iterable } of candidatePatterns) {
          const origin = iterable
            ? iterableElementOrigin(initializer)
            : expressionOrigin(initializer);
          if (origin !== 'root') continue;
          for (const property of pattern.properties) {
            if (property.type !== 'Property') continue;
            const propertyName = staticPatternPropertyName(property);
            if (ADTECH_GLOBALS.has(propertyName)) report(property, propertyName);
          }
        }

        for (const node of candidateMembers) {
          const propertyName = staticPropertyName(node);
          if (!ADTECH_GLOBALS.has(propertyName)) continue;
          if (expressionOrigin(node.object) === 'root') report(node, propertyName);
        }

        for (const node of candidateIdentifiers) {
          if (!isReference(node) || expressionOrigin(node) !== 'adtech') continue;
          report(node, node.name);
        }
      },
    };
  },
};

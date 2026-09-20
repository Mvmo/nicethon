// Nicethon runtime. Every generated module includes it, everything is static.
#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <frameobject.h>
#include <stddef.h>
#include <math.h>
#include <stdint.h>
#include <string.h>

enum { NT_I64, NT_F64, NT_BOOL, NT_OBJ };

#define NT_FAIL(line) do { nt_lineno = (line); goto error; } while (0)

static PyObject *nt_panic_type;
static PyObject *nt_frame_globals;
static PyTypeObject **nt_types;
static PyTypeObject *nt_pyerr_type;

static int nt_panic(PyObject *message) {
    PyErr_SetObject(nt_panic_type, message);
    return -1;
}

static int nt_panic_text(const char *message) {
    PyErr_SetString(nt_panic_type, message);
    return -1;
}

static void nt_traceback(const char *function, const char *file, int line) {
    if (line == 0) return;
    PyObject *raised = PyErr_GetRaisedException();
    PyCodeObject *code = PyCode_NewEmpty(file, function, line);
    if (code) {
        PyFrameObject *frame = PyFrame_New(PyThreadState_Get(), code, nt_frame_globals, NULL);
        PyErr_SetRaisedException(raised);
        raised = NULL;
        if (frame) {
            PyTraceBack_Here(frame);
            Py_DECREF(frame);
        }
        Py_DECREF(code);
    }
    if (raised) PyErr_SetRaisedException(raised);
}

static PyObject *nt_type_name(PyObject *value) {
    PyTypeObject *type = Py_TYPE(value);
    PyObject *module = PyObject_GetAttrString((PyObject *)type, "__module__");
    PyObject *name = PyObject_GetAttrString((PyObject *)type, "__qualname__");
    PyObject *result = NULL;
    if (module && name) {
        if (PyUnicode_Check(module) && PyUnicode_CompareWithASCIIString(module, "builtins") != 0)
            result = PyUnicode_FromFormat("%U.%U", module, name);
        else
            result = Py_NewRef(name);
    }
    Py_XDECREF(module);
    Py_XDECREF(name);
    PyErr_Clear();
    return result ? result : PyUnicode_FromString(type->tp_name);
}

// Records: structs, enum variants, Option, Result, PyErr.

typedef struct {
    const char *name;
    int kind;
    Py_ssize_t offset;
    const char *spec;
    const char *display;
} nt_Field;

typedef struct {
    PyTypeObject type;
    const char *show_name;
    int field_count;
    const nt_Field *fields;
    int enum_id;
} nt_RecordType;

typedef struct {
    PyObject_HEAD
    PyObject *value;
} nt_Payload;

#define NT_FIELD(record, field) (*(PyObject **)((char *)(record) + (field)->offset))
#define NT_RECORD(T) ((nt_RecordType *)Py_TYPE(T))

static void nt_record_dealloc(PyObject *self) {
    nt_RecordType *type = NT_RECORD(self);
    for (int i = 0; i < type->field_count; i++)
        if (type->fields[i].kind == NT_OBJ) Py_XDECREF(NT_FIELD(self, &type->fields[i]));
    Py_TYPE(self)->tp_free(self);
}

static int nt_is_record(PyObject *value) {
    return Py_TYPE(value)->tp_dealloc == nt_record_dealloc;
}

static PyObject *nt_record_alloc(PyTypeObject *type) {
    return type->tp_alloc(type, 0);
}

static PyObject *nt_field_get(PyObject *self, const nt_Field *field) {
    char *at = (char *)self + field->offset;
    switch (field->kind) {
    case NT_I64: return PyLong_FromLongLong(*(int64_t *)at);
    case NT_F64: return PyFloat_FromDouble(*(double *)at);
    case NT_BOOL: return PyBool_FromLong(*(int *)at);
    default: return Py_NewRef(*(PyObject **)at);
    }
}

static PyObject *nt_record_copy(PyObject *self) {
    nt_RecordType *type = NT_RECORD(self);
    PyObject *copy = nt_record_alloc(&type->type);
    if (!copy) return NULL;
    memcpy((char *)copy + sizeof(PyObject), (char *)self + sizeof(PyObject), type->type.tp_basicsize - sizeof(PyObject));
    for (int i = 0; i < type->field_count; i++)
        if (type->fields[i].kind == NT_OBJ) Py_XINCREF(NT_FIELD(copy, &type->fields[i]));
    return copy;
}

static PyObject *nt_make_pyerr(PyObject *exception) {
    PyObject *record = nt_record_alloc(nt_pyerr_type);
    if (!record) return NULL;
    PyObject **fields = &((nt_Payload *)record)->value;
    fields[0] = PyObject_Str(exception);
    fields[1] = PyUnicode_FromString(_PyType_Name(Py_TYPE(exception)));
    fields[2] = Py_NewRef(exception);
    if (!fields[0] || !fields[1]) {
        Py_DECREF(record);
        return NULL;
    }
    return record;
}

// Show: the text form used by print, templates and __repr__.

static int nt_show_into(PyObject *parts, PyObject *value, int nested);

static int nt_append_text(PyObject *parts, const char *text) {
    PyObject *piece = PyUnicode_FromString(text);
    if (!piece) return -1;
    int status = PyList_Append(parts, piece);
    Py_DECREF(piece);
    return status;
}

static int nt_append_object(PyObject *parts, PyObject *piece) {
    if (!piece) return -1;
    int status = PyList_Append(parts, piece);
    Py_DECREF(piece);
    return status;
}

static PyTypeObject nt_ListType;

typedef struct {
    PyObject_HEAD
    Py_ssize_t length, capacity;
    int kind;
    PyTypeObject *item_type;
    char *data;
} nt_List;

static PyObject *nt_list_item(PyObject *list, Py_ssize_t index);

static int nt_show_sequence(PyObject *parts, PyObject *items, const char *open, const char *close) {
    if (nt_append_text(parts, open) < 0) return -1;
    Py_ssize_t length = PySequence_Length(items);
    for (Py_ssize_t i = 0; i < length; i++) {
        if (i > 0 && nt_append_text(parts, ", ") < 0) return -1;
        PyObject *item = Py_IS_TYPE(items, &nt_ListType) ? nt_list_item(items, i) : PySequence_GetItem(items, i);
        if (!item) return -1;
        int status = nt_show_into(parts, item, 1);
        Py_DECREF(item);
        if (status < 0) return -1;
    }
    if (length == 1 && PyTuple_Check(items) && nt_append_text(parts, ",") < 0) return -1;
    return nt_append_text(parts, close);
}

static int nt_show_into(PyObject *parts, PyObject *value, int nested) {
    if (PyBool_Check(value)) return nt_append_text(parts, value == Py_True ? "true" : "false");
    if (PyFloat_Check(value)) {
        char *text = PyOS_double_to_string(PyFloat_AS_DOUBLE(value), 'r', 0, Py_DTSF_ADD_DOT_0, NULL);
        if (!text) return -1;
        int status = nt_append_text(parts, text);
        PyMem_Free(text);
        return status;
    }
    if (PyUnicode_Check(value)) {
        if (!nested) return PyList_Append(parts, value);
        PyObject *escaped = Py_NewRef(value);
        const char *pairs[][2] = {{"\\", "\\\\"}, {"\"", "\\\""}, {"\n", "\\n"}, {"\t", "\\t"}};
        for (int i = 0; escaped && i < 4; i++) Py_SETREF(escaped, PyObject_CallMethod(escaped, "replace", "ss", pairs[i][0], pairs[i][1]));
        if (!escaped) return -1;
        PyObject *quoted = PyUnicode_FromFormat("\"%U\"", escaped);
        Py_DECREF(escaped);
        return nt_append_object(parts, quoted);
    }
    if (Py_IS_TYPE(value, &nt_ListType)) return nt_show_sequence(parts, value, "[", "]");
    if (PyTuple_CheckExact(value)) return nt_show_sequence(parts, value, "(", ")");
    if (nt_is_record(value)) {
        nt_RecordType *type = NT_RECORD(value);
        if (nt_append_text(parts, type->show_name) < 0) return -1;
        if (type->field_count == 0) return 0;
        if (nt_append_text(parts, "(") < 0) return -1;
        for (int i = 0; i < type->field_count; i++) {
            const nt_Field *field = &type->fields[i];
            if (i > 0 && nt_append_text(parts, ", ") < 0) return -1;
            if (field->name[0] != '\0' && (nt_append_text(parts, field->name) < 0 || nt_append_text(parts, " = ") < 0)) return -1;
            if (field->spec[0] == 'u') {
                if (nt_append_text(parts, "()") < 0) return -1;
                continue;
            }
            PyObject *item = nt_field_get(value, field);
            if (!item) return -1;
            int status = nt_show_into(parts, item, 1);
            Py_DECREF(item);
            if (status < 0) return -1;
        }
        return nt_append_text(parts, ")");
    }
    return nt_append_object(parts, PyObject_Str(value));
}

static PyObject *nt_show(PyObject *value, int nested) {
    PyObject *parts = PyList_New(0);
    if (!parts) return NULL;
    PyObject *result = NULL;
    if (nt_show_into(parts, value, nested) == 0) {
        PyObject *empty = PyUnicode_FromString("");
        if (empty) result = PyUnicode_Join(empty, parts);
        Py_XDECREF(empty);
    }
    Py_DECREF(parts);
    return result;
}

static PyObject *nt_template(Py_ssize_t count, PyObject **values) {
    PyObject *parts = PyList_New(0);
    if (!parts) return NULL;
    for (Py_ssize_t i = 0; i < count; i++) {
        if (nt_show_into(parts, values[i], 0) < 0) {
            Py_DECREF(parts);
            return NULL;
        }
    }
    PyObject *empty = PyUnicode_FromString("");
    PyObject *result = empty ? PyUnicode_Join(empty, parts) : NULL;
    Py_XDECREF(empty);
    Py_DECREF(parts);
    return result;
}

static PyObject *nt_record_repr(PyObject *self) {
    return nt_show(self, 0);
}

static PyObject *nt_record_richcompare(PyObject *left, PyObject *right, int op) {
    if ((op != Py_EQ && op != Py_NE) || Py_TYPE(left) != Py_TYPE(right)) Py_RETURN_NOTIMPLEMENTED;
    nt_RecordType *type = NT_RECORD(left);
    int equal = 1;
    for (int i = 0; i < type->field_count && equal; i++) {
        const nt_Field *field = &type->fields[i];
        char *a = (char *)left + field->offset, *b = (char *)right + field->offset;
        switch (field->kind) {
        case NT_I64: equal = *(int64_t *)a == *(int64_t *)b; break;
        case NT_F64: equal = *(double *)a == *(double *)b; break;
        case NT_BOOL: equal = *(int *)a == *(int *)b; break;
        default:
            equal = PyObject_RichCompareBool(*(PyObject **)a, *(PyObject **)b, Py_EQ);
            if (equal < 0) return NULL;
        }
    }
    return PyBool_FromLong(op == Py_EQ ? equal : !equal);
}

static Py_hash_t nt_record_hash(PyObject *self) {
    nt_RecordType *type = NT_RECORD(self);
    Py_uhash_t hash = (Py_uhash_t)(uintptr_t)type;
    for (int i = 0; i < type->field_count; i++) {
        PyObject *item = nt_field_get(self, &type->fields[i]);
        if (!item) return -1;
        Py_hash_t part = PyObject_Hash(item);
        Py_DECREF(item);
        if (part == -1) return -1;
        hash = hash * 1000003u ^ (Py_uhash_t)part;
    }
    return hash == (Py_uhash_t)-1 ? -2 : (Py_hash_t)hash;
}

static PyObject *nt_record_getattro(PyObject *self, PyObject *name) {
    nt_RecordType *type = NT_RECORD(self);
    for (int i = 0; i < type->field_count; i++)
        if (type->fields[i].name[0] && PyUnicode_CompareWithASCIIString(name, type->fields[i].name) == 0)
            return nt_field_get(self, &type->fields[i]);
    return PyObject_GenericGetAttr(self, name);
}

static int nt_record_setattro(PyObject *self, PyObject *name, PyObject *value) {
    PyErr_Format(PyExc_AttributeError, "cannot assign to field '%U' of immutable struct %s", name, NT_RECORD(self)->show_name);
    return -1;
}

static PyObject *nt_from_py(PyObject *value, const char *spec, const char *display, const char *context);

// Stores a value that is already in Nicethon form.
static void nt_set_field(PyObject *record, const nt_Field *field, PyObject *value) {
    char *at = (char *)record + field->offset;
    switch (field->kind) {
    case NT_I64: *(int64_t *)at = PyLong_AsLongLong(value); break;
    case NT_F64: *(double *)at = PyFloat_AsDouble(value); break;
    case NT_BOOL: *(int *)at = value == Py_True; break;
    default: *(PyObject **)at = Py_NewRef(value);
    }
}

static int nt_store_field(PyObject *record, const nt_Field *field, PyObject *value) {
    PyObject *checked = nt_from_py(value, field->spec, field->display, field->name);
    if (!checked) return -1;
    nt_set_field(record, field, checked);
    Py_DECREF(checked);
    return 0;
}

static PyObject *nt_record_new(PyTypeObject *subtype, PyObject *args, PyObject *kwargs) {
    nt_RecordType *type = (nt_RecordType *)subtype;
    Py_ssize_t given = PyTuple_GET_SIZE(args);
    if (given > type->field_count) return PyErr_Format(PyExc_TypeError, "%s takes %d arguments", type->show_name, type->field_count);
    PyObject *record = nt_record_alloc(subtype);
    if (!record) return NULL;
    for (int i = 0; i < type->field_count; i++) {
        const nt_Field *field = &type->fields[i];
        PyObject *value = i < given ? PyTuple_GET_ITEM(args, i) : (kwargs ? PyDict_GetItemString(kwargs, field->name) : NULL);
        if (!value) {
            Py_DECREF(record);
            return PyErr_Format(PyExc_TypeError, "missing argument '%s'", field->name);
        }
        if (nt_store_field(record, field, value) < 0) {
            Py_DECREF(record);
            return NULL;
        }
    }
    return record;
}

#define NT_RECORD_TYPE(qualname, show, cstruct, fields, count, enum_id) \
    {{PyVarObject_HEAD_INIT(NULL, 0) .tp_name = qualname, .tp_basicsize = sizeof(cstruct), \
      .tp_flags = Py_TPFLAGS_DEFAULT, .tp_dealloc = nt_record_dealloc, .tp_repr = nt_record_repr, \
      .tp_hash = nt_record_hash, .tp_richcompare = nt_record_richcompare, .tp_getattro = nt_record_getattro, \
      .tp_setattro = nt_record_setattro, .tp_new = nt_record_new}, show, count, fields, enum_id}

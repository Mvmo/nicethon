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
    int exports;
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

// Lists: Int, Float and Bool elements are stored unboxed.

#define NT_SLOT(list, index) (((int64_t *)((nt_List *)(list))->data) + (index))

static PyObject *nt_list_new(int kind, Py_ssize_t capacity) {
    nt_List *list = PyObject_New(nt_List, &nt_ListType);
    if (!list) return NULL;
    list->length = 0;
    list->capacity = capacity > 4 ? capacity : 4;
    list->kind = kind;
    list->exports = 0;
    list->item_type = NULL;
    list->data = PyMem_Malloc(list->capacity * 8);
    if (!list->data) {
        Py_DECREF(list);
        return PyErr_NoMemory();
    }
    return (PyObject *)list;
}

static int nt_list_reserve(nt_List *list) {
    if (list->length < list->capacity) return 0;
    if (list->exports > 0) {
        PyErr_SetString(PyExc_BufferError, "cannot resize a list while a buffer view on it exists");
        return -1;
    }
    char *data = PyMem_Realloc(list->data, list->capacity * 2 * 8);
    if (!data) {
        PyErr_NoMemory();
        return -1;
    }
    list->data = data;
    list->capacity *= 2;
    return 0;
}

static int nt_list_index(PyObject *list, int64_t *index) {
    Py_ssize_t length = ((nt_List *)list)->length;
    if (*index < 0) *index += length;
    if (*index < 0 || *index >= length) {
        PyErr_Format(nt_panic_type, "index %lld out of range for length %zd", (long long)*index, length);
        return -1;
    }
    return 0;
}

static PyObject *nt_list_item(PyObject *list, Py_ssize_t index) {
    int64_t *slot = NT_SLOT(list, index);
    switch (((nt_List *)list)->kind) {
    case NT_I64: return PyLong_FromLongLong(*slot);
    case NT_F64: return PyFloat_FromDouble(*(double *)slot);
    case NT_BOOL: return PyBool_FromLong(*slot);
    default: return Py_NewRef(*(PyObject **)slot);
    }
}

static int nt_list_store(PyObject *list, Py_ssize_t index, PyObject *value, int fresh) {
    nt_List *self = (nt_List *)list;
    int64_t *slot = NT_SLOT(list, index);
    switch (self->kind) {
    case NT_I64: {
        int overflow = 0;
        long long number = PyLong_AsLongLongAndOverflow(value, &overflow);
        if (overflow) return PyErr_SetString(PyExc_OverflowError, "Int out of range"), -1;
        if (number == -1 && PyErr_Occurred()) return -1;
        *slot = number;
        return 0;
    }
    case NT_F64: {
        double number = PyFloat_AsDouble(value);
        if (number == -1.0 && PyErr_Occurred()) return -1;
        *(double *)slot = number;
        return 0;
    }
    case NT_BOOL: *slot = PyObject_IsTrue(value); return 0;
    default:
        if (fresh) *(PyObject **)slot = Py_NewRef(value);
        else Py_SETREF(*(PyObject **)slot, Py_NewRef(value));
        return 0;
    }
}

static int nt_list_check_item(nt_List *list, PyObject *value) {
    int ok;
    switch (list->kind) {
    case NT_I64: ok = PyLong_Check(value) && !PyBool_Check(value); break;
    case NT_F64: ok = PyFloat_Check(value) || (PyLong_Check(value) && !PyBool_Check(value)); break;
    case NT_BOOL: ok = PyBool_Check(value); break;
    default: ok = !list->item_type || PyObject_TypeCheck(value, list->item_type);
    }
    if (!ok) {
        PyObject *found = nt_type_name(value);
        PyErr_Format(PyExc_TypeError, "list item: found %U", found);
        Py_XDECREF(found);
    }
    return ok ? 0 : -1;
}

static int nt_list_append(PyObject *list, PyObject *value) {
    nt_List *self = (nt_List *)list;
    if (nt_list_reserve(self) < 0 || nt_list_store(list, self->length, value, 1) < 0) return -1;
    self->length++;
    return 0;
}

static int nt_list_append_i64(PyObject *list, int64_t value) {
    nt_List *self = (nt_List *)list;
    if (self->kind != NT_I64) {
        PyObject *boxed = self->kind == NT_F64 ? PyFloat_FromDouble((double)value) : PyLong_FromLongLong(value);
        int status = boxed ? nt_list_append(list, boxed) : -1;
        Py_XDECREF(boxed);
        return status;
    }
    if (nt_list_reserve(self) < 0) return -1;
    *NT_SLOT(list, self->length++) = value;
    return 0;
}

static int nt_list_append_f64(PyObject *list, double value) {
    nt_List *self = (nt_List *)list;
    if (self->kind != NT_F64) {
        PyObject *boxed = PyFloat_FromDouble(value);
        int status = boxed ? nt_list_append(list, boxed) : -1;
        Py_XDECREF(boxed);
        return status;
    }
    if (nt_list_reserve(self) < 0) return -1;
    *(double *)NT_SLOT(list, self->length++) = value;
    return 0;
}

static int nt_list_append_bool(PyObject *list, int value) {
    return nt_list_append(list, value ? Py_True : Py_False);
}

static int64_t nt_list_get_i64(PyObject *list, int64_t index) {
    if (nt_list_index(list, &index) < 0) return -1;
    if (((nt_List *)list)->kind == NT_I64) return *NT_SLOT(list, index);
    return PyLong_AsLongLong(*(PyObject **)NT_SLOT(list, index));
}

static double nt_list_get_f64(PyObject *list, int64_t index) {
    if (nt_list_index(list, &index) < 0) return -1.0;
    if (((nt_List *)list)->kind == NT_F64) return *(double *)NT_SLOT(list, index);
    return PyFloat_AsDouble(*(PyObject **)NT_SLOT(list, index));
}

static int nt_list_get_bool(PyObject *list, int64_t index) {
    if (nt_list_index(list, &index) < 0) return -1;
    if (((nt_List *)list)->kind == NT_BOOL) return (int)*NT_SLOT(list, index);
    return *(PyObject **)NT_SLOT(list, index) == Py_True;
}

static PyObject *nt_list_get_obj(PyObject *list, int64_t index) {
    if (nt_list_index(list, &index) < 0) return NULL;
    return nt_list_item(list, index);
}

static int nt_list_set(PyObject *list, int64_t index, PyObject *value) {
    if (nt_list_index(list, &index) < 0) return -1;
    return nt_list_store(list, index, value, 0);
}

static int nt_list_set_i64(PyObject *list, int64_t index, int64_t value) {
    if (nt_list_index(list, &index) < 0) return -1;
    if (((nt_List *)list)->kind == NT_I64) {
        *NT_SLOT(list, index) = value;
        return 0;
    }
    PyObject *boxed = PyLong_FromLongLong(value);
    int status = boxed ? nt_list_store(list, index, boxed, 0) : -1;
    Py_XDECREF(boxed);
    return status;
}

static int nt_list_set_f64(PyObject *list, int64_t index, double value) {
    if (nt_list_index(list, &index) < 0) return -1;
    if (((nt_List *)list)->kind == NT_F64) {
        *(double *)NT_SLOT(list, index) = value;
        return 0;
    }
    PyObject *boxed = PyFloat_FromDouble(value);
    int status = boxed ? nt_list_store(list, index, boxed, 0) : -1;
    Py_XDECREF(boxed);
    return status;
}

static int nt_list_set_bool(PyObject *list, int64_t index, int value) {
    return nt_list_set(list, index, value ? Py_True : Py_False);
}

static Py_ssize_t nt_list_len(PyObject *list) {
    return ((nt_List *)list)->length;
}

static PyObject *nt_range(int64_t start, int64_t end) {
    PyObject *list = nt_list_new(NT_I64, end > start ? end - start : 0);
    if (!list) return NULL;
    for (int64_t i = start; i < end; i++) *NT_SLOT(list, i - start) = i;
    ((nt_List *)list)->length = end > start ? end - start : 0;
    return list;
}

static PyObject *nt_list_concat(PyObject *left, PyObject *right) {
    nt_List *a = (nt_List *)left;
    PyObject *result = nt_list_new(a->kind, a->length + ((nt_List *)right)->length);
    if (!result) return NULL;
    PyObject *sides[2] = {left, right};
    for (int side = 0; side < 2; side++) {
        for (Py_ssize_t i = 0; i < ((nt_List *)sides[side])->length; i++) {
            PyObject *item = nt_list_item(sides[side], i);
            int status = item ? nt_list_append(result, item) : -1;
            Py_XDECREF(item);
            if (status < 0) {
                Py_DECREF(result);
                return NULL;
            }
        }
    }
    return result;
}

static void nt_list_dealloc(PyObject *self) {
    nt_List *list = (nt_List *)self;
    if (list->kind == NT_OBJ)
        for (Py_ssize_t i = 0; i < list->length; i++) Py_DECREF(*(PyObject **)NT_SLOT(self, i));
    PyMem_Free(list->data);
    PyObject_Free(self);
}

static Py_ssize_t nt_list_sq_length(PyObject *self) {
    return ((nt_List *)self)->length;
}

static PyObject *nt_list_sq_item(PyObject *self, Py_ssize_t index) {
    if (index < 0 || index >= ((nt_List *)self)->length) {
        PyErr_SetString(PyExc_IndexError, "list index out of range");
        return NULL;
    }
    return nt_list_item(self, index);
}

static int nt_list_sq_ass_item(PyObject *self, Py_ssize_t index, PyObject *value) {
    if (!value) {
        PyErr_SetString(PyExc_TypeError, "Nicethon lists do not support item deletion");
        return -1;
    }
    if (index < 0 || index >= ((nt_List *)self)->length) {
        PyErr_SetString(PyExc_IndexError, "list index out of range");
        return -1;
    }
    if (nt_list_check_item((nt_List *)self, value) < 0) return -1;
    return nt_list_store(self, index, value, 0);
}

static int nt_list_sq_contains(PyObject *self, PyObject *value) {
    for (Py_ssize_t i = 0; i < ((nt_List *)self)->length; i++) {
        PyObject *item = nt_list_item(self, i);
        if (!item) return -1;
        int equal = PyObject_RichCompareBool(item, value, Py_EQ);
        Py_DECREF(item);
        if (equal != 0) return equal;
    }
    return 0;
}

static PyObject *nt_list_py_append(PyObject *self, PyObject *value) {
    if (nt_list_check_item((nt_List *)self, value) < 0 || nt_list_append(self, value) < 0) return NULL;
    Py_RETURN_NONE;
}

static PyObject *nt_list_richcompare(PyObject *left, PyObject *right, int op) {
    if ((op != Py_EQ && op != Py_NE) || !Py_IS_TYPE(right, &nt_ListType)) Py_RETURN_NOTIMPLEMENTED;
    nt_List *a = (nt_List *)left, *b = (nt_List *)right;
    int equal = a->length == b->length;
    for (Py_ssize_t i = 0; equal && i < a->length; i++) {
        PyObject *x = nt_list_item(left, i), *y = nt_list_item(right, i);
        equal = x && y ? PyObject_RichCompareBool(x, y, Py_EQ) : -1;
        Py_XDECREF(x);
        Py_XDECREF(y);
        if (equal < 0) return NULL;
    }
    return PyBool_FromLong(op == Py_EQ ? equal : !equal);
}

static int nt_list_getbuffer(PyObject *self, Py_buffer *view, int flags) {
    nt_List *list = (nt_List *)self;
    if (list->kind != NT_I64 && list->kind != NT_F64) {
        PyErr_SetString(PyExc_BufferError, "only lists of Int or Float expose a buffer");
        return -1;
    }
    view->obj = Py_NewRef(self);
    view->buf = list->data;
    view->len = list->length * 8;
    view->readonly = 0;
    view->itemsize = 8;
    view->format = (flags & PyBUF_FORMAT) ? (list->kind == NT_I64 ? "q" : "d") : NULL;
    view->ndim = 1;
    view->shape = (flags & PyBUF_ND) == PyBUF_ND ? &view->len : NULL;
    view->strides = (flags & PyBUF_STRIDES) == PyBUF_STRIDES ? &view->itemsize : NULL;
    view->suboffsets = NULL;
    view->internal = NULL;
    list->exports++;
    return 0;
}

static void nt_list_releasebuffer(PyObject *self, Py_buffer *view) {
    ((nt_List *)self)->exports--;
}

static PyObject *nt_list_repr(PyObject *self) {
    return nt_show(self, 0);
}

static PySequenceMethods nt_list_sequence = {
    .sq_length = nt_list_sq_length,
    .sq_item = nt_list_sq_item,
    .sq_ass_item = nt_list_sq_ass_item,
    .sq_contains = nt_list_sq_contains,
};

static PyBufferProcs nt_list_buffer = {nt_list_getbuffer, nt_list_releasebuffer};

static PyMethodDef nt_list_methods[] = {
    {"append", nt_list_py_append, METH_O, NULL},
    {NULL, NULL, 0, NULL},
};

static PyTypeObject nt_ListType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "nicethon.List",
    .tp_basicsize = sizeof(nt_List),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_SEQUENCE,
    .tp_dealloc = nt_list_dealloc,
    .tp_repr = nt_list_repr,
    .tp_as_sequence = &nt_list_sequence,
    .tp_as_buffer = &nt_list_buffer,
    .tp_richcompare = nt_list_richcompare,
    .tp_iter = PySeqIter_New,
    .tp_methods = nt_list_methods,
};

// Boundary checks: a spec string describes the expected type.
// i Int, f Float, b Bool, s Str, y Bytes, n BigInt, u Unit, d Dyn, F function,
// L<item> List, S<item> Set, M<key><value> Map, O<some>;<none>; Option, T<count>;<items> tuple,
// r<index>; record type, e<index>; enum.

static int nt_spec_index(const char **spec) {
    int index = 0;
    while (**spec >= '0' && **spec <= '9') index = index * 10 + *(*spec)++ - '0';
    if (**spec == ';') (*spec)++;
    return index;
}

static void nt_skip_spec(const char **spec) {
    char c = *(*spec)++;
    switch (c) {
    case 'L': case 'S': nt_skip_spec(spec); break;
    case 'M': nt_skip_spec(spec); nt_skip_spec(spec); break;
    case 'T': for (int n = nt_spec_index(spec); n > 0; n--) nt_skip_spec(spec); break;
    case 'r': case 'e': nt_spec_index(spec); break;
    case 'O': nt_spec_index(spec); nt_spec_index(spec); break;
    }
}

static int nt_spec_kind(const char *spec) {
    return spec[0] == 'i' ? NT_I64 : spec[0] == 'f' ? NT_F64 : spec[0] == 'b' ? NT_BOOL : NT_OBJ;
}

static PyTypeObject *nt_spec_type(const char *spec) {
    if (spec[0] == 's') return &PyUnicode_Type;
    if (spec[0] == 'r') {
        const char *rest = spec + 1;
        return nt_types[nt_spec_index(&rest)];
    }
    return NULL;
}

// Returns a new reference in Nicethon form. NULL without an error set means a
// type mismatch, NULL with an error set means another failure.
static PyObject *nt_convert(PyObject *value, const char **spec) {
    char c = *(*spec)++;
    switch (c) {
    case 'd': return Py_NewRef(value);
    case 'u': return value == Py_None ? Py_NewRef(value) : NULL;
    case 'i': case 'n': {
        if (PyBool_Check(value) || PyFloat_Check(value)) return NULL;
        if (PyLong_CheckExact(value)) {
            if (c == 'i') {
                int overflow = 0;
                PyLong_AsLongLongAndOverflow(value, &overflow);
                if (overflow) return NULL;
            }
            return Py_NewRef(value);
        }
        if (!PyIndex_Check(value)) return NULL;
        PyObject *number = PyNumber_Index(value);
        if (!number) return NULL;
        int overflow = 0;
        if (c == 'i') PyLong_AsLongLongAndOverflow(number, &overflow);
        if (overflow) {
            Py_DECREF(number);
            return NULL;
        }
        return number;
    }
    case 'f':
        if (PyFloat_CheckExact(value)) return Py_NewRef(value);
        if (PyBool_Check(value)) return NULL;
        if (PyFloat_Check(value) || PyLong_Check(value) || Py_TYPE(value)->tp_as_number && Py_TYPE(value)->tp_as_number->nb_float) {
            double number = PyFloat_AsDouble(value);
            if (number == -1.0 && PyErr_Occurred()) {
                PyErr_Clear();
                return NULL;
            }
            return PyFloat_FromDouble(number);
        }
        return NULL;
    case 'b':
        if (PyBool_Check(value)) return Py_NewRef(value);
        if (strcmp(Py_TYPE(value)->tp_name, "numpy.bool") == 0 || strcmp(Py_TYPE(value)->tp_name, "numpy.bool_") == 0)
            return Py_NewRef(PyObject_IsTrue(value) ? Py_True : Py_False);
        return NULL;
    case 's': return PyUnicode_Check(value) ? Py_NewRef(value) : NULL;
    case 'y': return PyBytes_Check(value) ? Py_NewRef(value) : NULL;
    case 'F': return PyCallable_Check(value) ? Py_NewRef(value) : NULL;
    case 'r': {
        PyTypeObject *type = nt_types[nt_spec_index(spec)];
        return Py_IS_TYPE(value, type) ? Py_NewRef(value) : NULL;
    }
    case 'e': {
        int id = nt_spec_index(spec);
        return nt_is_record(value) && NT_RECORD(value)->enum_id == id ? Py_NewRef(value) : NULL;
    }
    case 'O': {
        PyTypeObject *some = nt_types[nt_spec_index(spec)], *none = nt_types[nt_spec_index(spec)];
        if (Py_IS_TYPE(value, some) || Py_IS_TYPE(value, none)) return Py_NewRef(value);
        if (value == Py_None) return nt_record_alloc(none);
        const nt_Field *field = &((nt_RecordType *)some)->fields[0];
        const char *inner = field->spec;
        PyObject *item = nt_convert(value, &inner);
        PyObject *record = item ? nt_record_alloc(some) : NULL;
        if (record) nt_set_field(record, field, item);
        Py_XDECREF(item);
        return record;
    }
    case 'T': {
        int count = nt_spec_index(spec);
        if (!PyTuple_Check(value) || PyTuple_GET_SIZE(value) != count) {
            for (int i = 0; i < count; i++) nt_skip_spec(spec);
            return NULL;
        }
        PyObject *tuple = PyTuple_New(count);
        for (int i = 0; tuple && i < count; i++) {
            PyObject *item = nt_convert(PyTuple_GET_ITEM(value, i), spec);
            if (!item) {
                Py_CLEAR(tuple);
                for (i++; i < count; i++) nt_skip_spec(spec);
                break;
            }
            PyTuple_SET_ITEM(tuple, i, item);
        }
        return tuple;
    }
    case 'L': {
        const char *inner = *spec;
        nt_skip_spec(spec);
        int kind = nt_spec_kind(inner);
        PyTypeObject *item_type = nt_spec_type(inner);
        if (Py_IS_TYPE(value, &nt_ListType)) {
            nt_List *list = (nt_List *)value;
            if (list->kind == kind && (inner[0] == 'd' || kind != NT_OBJ || (item_type && list->item_type == item_type)))
                return Py_NewRef(value);
        }
        if (!PyList_Check(value) && !PyTuple_Check(value) && !Py_IS_TYPE(value, &nt_ListType)) return NULL;
        Py_ssize_t length = PySequence_Length(value);
        PyObject *list = nt_list_new(kind, length);
        if (!list) return NULL;
        ((nt_List *)list)->item_type = item_type;
        for (Py_ssize_t i = 0; i < length; i++) {
            PyObject *raw = PySequence_GetItem(value, i);
            const char *cursor = inner;
            PyObject *item = raw ? nt_convert(raw, &cursor) : NULL;
            Py_XDECREF(raw);
            if (!item || nt_list_append(list, item) < 0) {
                Py_XDECREF(item);
                Py_DECREF(list);
                return NULL;
            }
            Py_DECREF(item);
        }
        return list;
    }
    case 'M': case 'S': {
        nt_skip_spec(spec);
        if (c == 'M') nt_skip_spec(spec);
        if (c == 'M' ? PyDict_Check(value) : PyAnySet_Check(value)) return Py_NewRef(value);
        return NULL;
    }
    }
    return NULL;
}

static PyObject *nt_from_py(PyObject *value, const char *spec, const char *display, const char *context) {
    const char *cursor = spec;
    PyObject *result = nt_convert(value, &cursor);
    if (result || PyErr_Occurred()) return result;
    PyObject *found = nt_type_name(value);
    if (context && context[0]) PyErr_Format(PyExc_TypeError, "argument '%s': expected %s, found %U", context, display, found);
    else PyErr_Format(PyExc_TypeError, "expected %s, found %U", display, found);
    Py_XDECREF(found);
    return NULL;
}

static PyObject *nt_try_convert(PyObject *value, const char *spec) {
    const char *cursor = spec;
    PyObject *result = nt_convert(value, &cursor);
    PyErr_Clear();
    return result;
}

static PyObject *nt_option_to_py(PyObject *option) {
    nt_RecordType *type = NT_RECORD(option);
    return type->field_count == 0 ? Py_NewRef(Py_None) : nt_field_get(option, &type->fields[0]);
}

// Function calls:

static int nt_args(PyObject *const *args, Py_ssize_t nargs, PyObject *kwnames, const char *const *names, int count, PyObject **slots) {
    if (nargs > count) {
        PyErr_Format(PyExc_TypeError, "takes %d arguments but %zd were given", count, nargs);
        return -1;
    }
    for (Py_ssize_t i = 0; i < nargs; i++) slots[i] = args[i];
    Py_ssize_t keywords = kwnames ? PyTuple_GET_SIZE(kwnames) : 0;
    for (Py_ssize_t k = 0; k < keywords; k++) {
        PyObject *name = PyTuple_GET_ITEM(kwnames, k);
        int found = -1;
        for (int i = 0; i < count; i++)
            if (PyUnicode_CompareWithASCIIString(name, names[i]) == 0) found = i;
        if (found < 0 || slots[found]) {
            PyErr_Format(PyExc_TypeError, found < 0 ? "unexpected argument '%U'" : "argument '%U' given twice", name);
            return -1;
        }
        slots[found] = args[nargs + k];
    }
    return 0;
}

// Exceptions:

static int nt_throw(PyObject *value) {
    if (nt_pyerr_type && Py_IS_TYPE(value, nt_pyerr_type)) value = (&((nt_Payload *)value)->value)[2];
    if (PyExceptionInstance_Check(value)) PyErr_SetObject((PyObject *)Py_TYPE(value), value);
    else if (PyExceptionClass_Check(value)) PyErr_SetNone(value);
    else PyErr_SetString(PyExc_TypeError, "can only throw python exceptions");
    return -1;
}

static int nt_matches(PyObject *exception, PyObject *filter) {
    return PyErr_GivenExceptionMatches(exception, filter ? filter : PyExc_Exception);
}

static PyObject *nt_with_exit(PyObject *exit, PyObject *exception) {
    if (!exception) return PyObject_CallFunctionObjArgs(exit, Py_None, Py_None, Py_None, NULL);
    PyObject *traceback = PyException_GetTraceback(exception);
    PyObject *result = PyObject_CallFunctionObjArgs(exit, (PyObject *)Py_TYPE(exception), exception, traceback ? traceback : Py_None, NULL);
    Py_XDECREF(traceback);
    return result;
}

// Integer and float arithmetic with Python semantics:

static int nt_overflow(void) {
    return nt_panic_text("integer overflow");
}

static int nt_floordiv_i64(int64_t left, int64_t right, int64_t *out) {
    if (right == 0) return nt_panic_text("integer division by zero");
    if (left == INT64_MIN && right == -1) return nt_overflow();
    int64_t quotient = left / right;
    if ((left % right != 0) && ((left < 0) != (right < 0))) quotient--;
    *out = quotient;
    return 0;
}

static int nt_mod_i64(int64_t left, int64_t right, int64_t *out) {
    if (right == 0) return nt_panic_text("integer modulo by zero");
    if (right == -1) {
        *out = 0;
        return 0;
    }
    int64_t rest = left % right;
    if (rest != 0 && ((rest < 0) != (right < 0))) rest += right;
    *out = rest;
    return 0;
}

static int nt_pow_i64(int64_t base, int64_t exponent, int64_t *out) {
    if (exponent < 0) return nt_panic_text("negative exponent for Int");
    int64_t result = 1;
    while (exponent > 0) {
        if ((exponent & 1) && __builtin_mul_overflow(result, base, &result)) return nt_overflow();
        exponent >>= 1;
        if (exponent > 0 && __builtin_mul_overflow(base, base, &base)) return nt_overflow();
    }
    *out = result;
    return 0;
}

// Same algorithm as CPython's float divmod, so ~/ and % match Python exactly,
// signed zeros included. Division by zero gives nan instead of raising.
static double nt_divmod_f64(double left, double right, double *rest) {
    *rest = fmod(left, right);
    double quotient = (left - *rest) / right;
    if (*rest) {
        if ((right < 0) != (*rest < 0)) {
            *rest += right;
            quotient -= 1.0;
        }
    } else {
        *rest = copysign(0.0, right);
    }
    if (!quotient) return copysign(0.0, left / right);
    double floored = floor(quotient);
    return quotient - floored > 0.5 ? floored + 1.0 : floored;
}

static double nt_mod_f64(double left, double right) {
    double rest;
    nt_divmod_f64(left, right, &rest);
    return right == 0 ? NAN : rest;
}

static double nt_floordiv_f64(double left, double right) {
    double rest;
    double quotient = nt_divmod_f64(left, right, &rest);
    return right == 0 ? NAN : quotient;
}

static int nt_float_to_int(double value, int64_t *out) {
    if (!(value > -9223372036854775809.0 && value < 9223372036854775808.0)) return nt_panic_text("Float out of range for Int");
    *out = (int64_t)value;
    return 0;
}

static int64_t nt_big_to_i64(PyObject *value) {
    int overflow = 0;
    long long result = PyLong_AsLongLongAndOverflow(value, &overflow);
    if (overflow) return nt_panic_text("BigInt out of range for Int");
    return result;
}

// Coroutines: an async function compiles to a resumable body function.

typedef PyObject *(*nt_Resume)(void *frame, PyObject *sent, int *yielded);

typedef struct {
    PyObject_HEAD
    nt_Resume resume;
    void (*clear)(void *frame);
    void *frame;
    int done;
} nt_Coroutine;

static PyTypeObject nt_CoroutineType;

static PyObject *nt_coroutine_new(nt_Resume resume, void (*clear)(void *), void *frame) {
    nt_Coroutine *coroutine = PyObject_New(nt_Coroutine, &nt_CoroutineType);
    if (!coroutine) {
        clear(frame);
        PyMem_Free(frame);
        return NULL;
    }
    coroutine->resume = resume;
    coroutine->clear = clear;
    coroutine->frame = frame;
    coroutine->done = 0;
    return (PyObject *)coroutine;
}

static PySendResult nt_coroutine_am_send(PyObject *self, PyObject *sent, PyObject **result) {
    nt_Coroutine *coroutine = (nt_Coroutine *)self;
    if (coroutine->done) {
        PyErr_SetString(PyExc_RuntimeError, "cannot reuse already awaited coroutine");
        *result = NULL;
        return PYGEN_ERROR;
    }
    int yielded = 0;
    PyObject *value = coroutine->resume(coroutine->frame, sent ? sent : Py_None, &yielded);
    if (yielded) {
        *result = value;
        return PYGEN_NEXT;
    }
    coroutine->done = 1;
    *result = value;
    return value ? PYGEN_RETURN : PYGEN_ERROR;
}

static PyObject *nt_coroutine_send(PyObject *self, PyObject *sent) {
    PyObject *result;
    PySendResult status = nt_coroutine_am_send(self, sent, &result);
    if (status == PYGEN_RETURN) {
        if (result == Py_None) PyErr_SetNone(PyExc_StopIteration);
        else {
            PyObject *stop = PyObject_CallOneArg(PyExc_StopIteration, result);
            if (stop) {
                PyErr_SetObject(PyExc_StopIteration, stop);
                Py_DECREF(stop);
            }
        }
        Py_DECREF(result);
        return NULL;
    }
    return result;
}

static PyObject *nt_coroutine_next(PyObject *self) {
    return nt_coroutine_send(self, Py_None);
}

static PyObject *nt_coroutine_throw(PyObject *self, PyObject *const *args, Py_ssize_t nargs) {
    nt_Coroutine *coroutine = (nt_Coroutine *)self;
    coroutine->done = 1;
    if (nargs > 0) nt_throw(args[nargs > 1 ? 1 : 0] != Py_None ? args[nargs > 1 ? 1 : 0] : args[0]);
    return NULL;
}

static PyObject *nt_coroutine_close(PyObject *self, PyObject *unused) {
    ((nt_Coroutine *)self)->done = 1;
    Py_RETURN_NONE;
}

static PyObject *nt_coroutine_await(PyObject *self) {
    return Py_NewRef(self);
}

static void nt_coroutine_dealloc(PyObject *self) {
    nt_Coroutine *coroutine = (nt_Coroutine *)self;
    coroutine->clear(coroutine->frame);
    PyMem_Free(coroutine->frame);
    PyObject_Free(self);
}

static PyAsyncMethods nt_coroutine_async = {.am_await = nt_coroutine_await, .am_send = nt_coroutine_am_send};

static PyMethodDef nt_coroutine_methods[] = {
    {"send", nt_coroutine_send, METH_O, NULL},
    {"throw", (PyCFunction)(void (*)(void))nt_coroutine_throw, METH_FASTCALL, NULL},
    {"close", nt_coroutine_close, METH_NOARGS, NULL},
    {"__await__", (PyCFunction)(void (*)(void))nt_coroutine_await, METH_NOARGS, NULL},
    {NULL, NULL, 0, NULL},
};

static PyTypeObject nt_CoroutineType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "nicethon.Coroutine",
    .tp_basicsize = sizeof(nt_Coroutine),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_dealloc = nt_coroutine_dealloc,
    .tp_as_async = &nt_coroutine_async,
    .tp_iter = nt_coroutine_await,
    .tp_iternext = nt_coroutine_next,
    .tp_methods = nt_coroutine_methods,
};

// Awaiting drives the awaited object's iterator one step per resume.
// Returns 1 when it yielded, 0 when it finished (both set *result), -1 on error.
static int nt_await_step(PyObject **iterator, PyObject *awaitable, PyObject *sent, PyObject **result) {
    if (!*iterator) {
        unaryfunc await = Py_TYPE(awaitable)->tp_as_async ? Py_TYPE(awaitable)->tp_as_async->am_await : NULL;
        if (!await) {
            PyErr_Format(PyExc_TypeError, "object %s can't be used in 'await' expression", Py_TYPE(awaitable)->tp_name);
            return -1;
        }
        *iterator = await(awaitable);
        if (!*iterator) return -1;
        sent = Py_None;
    }
    PySendResult status = PyIter_Send(*iterator, sent, result);
    if (status == PYGEN_ERROR) {
        Py_CLEAR(*iterator);
        return -1;
    }
    if (status == PYGEN_RETURN) {
        Py_CLEAR(*iterator);
        return 0;
    }
    return 1;
}

// Intrinsics declared in the prelude:

static int nt_print(PyObject *value) {
    PyObject *text = nt_show(value, 0);
    if (!text) return -1;
    PyObject *out = PySys_GetObject("stdout");
    int status = out ? PyFile_WriteObject(text, out, Py_PRINT_RAW) : -1;
    Py_DECREF(text);
    if (status == 0) status = PyFile_WriteString("\n", out);
    return status;
}

static PyObject *nt_str(PyObject *value) {
    return nt_show(value, 0);
}

static PyObject *nt_sum(PyObject *items) {
    nt_List *list = (nt_List *)items;
    if (list->kind == NT_I64) {
        int64_t total = 0;
        for (Py_ssize_t i = 0; i < list->length; i++)
            if (__builtin_add_overflow(total, *NT_SLOT(items, i), &total)) return nt_overflow(), NULL;
        return PyLong_FromLongLong(total);
    }
    if (list->kind == NT_F64) {
        double total = 0;
        for (Py_ssize_t i = 0; i < list->length; i++) total += *(double *)NT_SLOT(items, i);
        return PyFloat_FromDouble(total);
    }
    PyObject *total = PyLong_FromLong(0);
    for (Py_ssize_t i = 0; total && i < list->length; i++) Py_SETREF(total, PyNumber_Add(total, *(PyObject **)NT_SLOT(items, i)));
    return total;
}

static int64_t nt_List_len(PyObject *self) {
    return ((nt_List *)self)->length;
}

static int nt_List_push(PyObject *self, PyObject *item) {
    return nt_list_append(self, item);
}

// Intrinsics returning an Option give the boxed value, or NULL without an error for None.

static PyObject *nt_List_pop(PyObject *self) {
    nt_List *list = (nt_List *)self;
    if (list->length == 0) return NULL;
    if (list->exports > 0) {
        PyErr_SetString(PyExc_BufferError, "cannot resize a list while a buffer view on it exists");
        return NULL;
    }
    PyObject *item = nt_list_item(self, list->length - 1);
    if (!item) return NULL;
    if (list->kind == NT_OBJ) Py_DECREF(*(PyObject **)NT_SLOT(self, list->length - 1));
    list->length--;
    return item;
}

static PyObject *nt_list_from_python_list(PyObject *items, int kind) {
    PyObject *list = nt_list_new(kind, PyList_GET_SIZE(items));
    for (Py_ssize_t i = 0; list && i < PyList_GET_SIZE(items); i++)
        if (nt_list_append(list, PyList_GET_ITEM(items, i)) < 0) Py_CLEAR(list);
    return list;
}

static PyObject *nt_sorted(PyObject *self, PyObject *key, int reverse) {
    PyObject *builtins = PyEval_GetBuiltins();
    PyObject *sorted = PyDict_GetItemString(builtins, "sorted");
    PyObject *args = PyTuple_Pack(1, self);
    PyObject *kwargs = PyDict_New();
    PyObject *result = NULL;
    if (args && kwargs && (!key || PyDict_SetItemString(kwargs, "key", key) == 0) && (!reverse || PyDict_SetItemString(kwargs, "reverse", Py_True) == 0)) {
        PyObject *items = PyObject_Call(sorted, args, kwargs);
        if (items) {
            result = nt_list_from_python_list(items, ((nt_List *)self)->kind);
            Py_DECREF(items);
        }
    }
    Py_XDECREF(args);
    Py_XDECREF(kwargs);
    return result;
}

static PyObject *nt_List_sort_by(PyObject *self, PyObject *key) {
    return nt_sorted(self, key, 0);
}

static PyObject *nt_List_sorted(PyObject *self) {
    return nt_sorted(self, NULL, 0);
}

static PyObject *nt_List_reversed(PyObject *self) {
    nt_List *list = (nt_List *)self;
    PyObject *result = nt_list_new(list->kind, list->length);
    for (Py_ssize_t i = list->length - 1; result && i >= 0; i--) {
        PyObject *item = nt_list_item(self, i);
        if (!item || nt_list_append(result, item) < 0) Py_CLEAR(result);
        Py_XDECREF(item);
    }
    return result;
}

static PyObject *nt_List_join(PyObject *self, PyObject *separator) {
    PyObject *parts = PyList_New(0);
    for (Py_ssize_t i = 0; parts && i < ((nt_List *)self)->length; i++) {
        PyObject *item = nt_list_item(self, i);
        PyObject *text = item ? nt_show(item, 0) : NULL;
        Py_XDECREF(item);
        if (!text || PyList_Append(parts, text) < 0) Py_CLEAR(parts);
        Py_XDECREF(text);
    }
    PyObject *result = parts ? PyUnicode_Join(separator, parts) : NULL;
    Py_XDECREF(parts);
    return result;
}

static int64_t nt_Str_len(PyObject *self) {
    return PyUnicode_GetLength(self);
}

static PyObject *nt_Str_upper(PyObject *self) {
    return PyObject_CallMethod(self, "upper", NULL);
}

static PyObject *nt_Str_lower(PyObject *self) {
    return PyObject_CallMethod(self, "lower", NULL);
}

static PyObject *nt_Str_trim(PyObject *self) {
    return PyObject_CallMethod(self, "strip", NULL);
}

static PyObject *nt_Str_replace(PyObject *self, PyObject *old, PyObject *new) {
    return PyUnicode_Replace(self, old, new, -1);
}

static PyObject *nt_Str_split(PyObject *self, PyObject *separator) {
    PyObject *parts = PyUnicode_Split(self, separator, -1);
    if (!parts) return NULL;
    PyObject *list = nt_list_from_python_list(parts, NT_OBJ);
    if (list) ((nt_List *)list)->item_type = &PyUnicode_Type;
    Py_DECREF(parts);
    return list;
}

static int nt_Str_starts_with(PyObject *self, PyObject *prefix) {
    return (int)PyUnicode_Tailmatch(self, prefix, 0, PY_SSIZE_T_MAX, -1);
}

static int nt_Str_ends_with(PyObject *self, PyObject *suffix) {
    return (int)PyUnicode_Tailmatch(self, suffix, 0, PY_SSIZE_T_MAX, 1);
}

static PyObject *nt_parsed(PyObject *number, int int_range) {
    if (!number) {
        if (PyErr_ExceptionMatches(PyExc_ValueError)) PyErr_Clear();
        return NULL;
    }
    int overflow = 0;
    if (int_range) PyLong_AsLongLongAndOverflow(number, &overflow);
    if (overflow) Py_CLEAR(number);
    return number;
}

static PyObject *nt_Str_parse_int(PyObject *self) {
    return nt_parsed(PyLong_FromUnicodeObject(self, 10), 1);
}

static PyObject *nt_Str_parse_float(PyObject *self) {
    return nt_parsed(PyFloat_FromString(self), 0);
}

static double nt_Float_floor(double self) { return floor(self); }
static double nt_Float_ceil(double self) { return ceil(self); }
static double nt_Float_sqrt(double self) { return sqrt(self); }
static double nt_Float_abs(double self) { return fabs(self); }
static double nt_Float_round(double self) { return round(self); }

static int64_t nt_Map_len(PyObject *self) {
    return PyDict_GET_SIZE(self);
}

static PyObject *nt_Map_get(PyObject *self, PyObject *key) {
    return Py_XNewRef(PyDict_GetItemWithError(self, key));
}

static PyObject *nt_Map_remove(PyObject *self, PyObject *key) {
    PyObject *value = Py_XNewRef(PyDict_GetItemWithError(self, key));
    if (value && PyDict_DelItem(self, key) < 0) Py_CLEAR(value);
    return value;
}

static PyObject *nt_Map_keys(PyObject *self) {
    PyObject *keys = PyDict_Keys(self);
    PyObject *list = keys ? nt_list_from_python_list(keys, NT_OBJ) : NULL;
    Py_XDECREF(keys);
    return list;
}

static PyObject *nt_Map_values(PyObject *self) {
    PyObject *values = PyDict_Values(self);
    PyObject *list = values ? nt_list_from_python_list(values, NT_OBJ) : NULL;
    Py_XDECREF(values);
    return list;
}

static PyObject *nt_map_get(PyObject *map, PyObject *key) {
    PyObject *value = PyDict_GetItemWithError(map, key);
    if (!value) {
        if (!PyErr_Occurred()) PyErr_SetObject(PyExc_KeyError, key);
        return NULL;
    }
    return Py_NewRef(value);
}

// Module setup shared by all generated modules:

static int nt_init_runtime(void) {
    if (PyType_Ready(&nt_ListType) < 0 || PyType_Ready(&nt_CoroutineType) < 0 || PyType_Ready(&nt_ClosureType) < 0) return -1;
    PyObject *shared = PyImport_AddModule("_nicethon");
    if (!shared) return -1;
    nt_panic_type = PyObject_GetAttrString(shared, "Panic");
    if (!nt_panic_type) {
        PyErr_Clear();
        nt_panic_type = PyErr_NewException("nicethon.Panic", PyExc_BaseException, NULL);
        if (!nt_panic_type || PyObject_SetAttrString(shared, "Panic", nt_panic_type) < 0) return -1;
    }
    nt_frame_globals = PyDict_New();
    return nt_frame_globals ? 0 : -1;
}

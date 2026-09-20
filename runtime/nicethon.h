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

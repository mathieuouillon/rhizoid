// ROOT's own reading of an RNTuple, condensed: for every top-level field of
// numbers (or a vector of numbers), the number of values and their sum,
// read through RDataFrame. `tests/cases` take their expected values from
// it (`tests/cross/make_cases.py`).
//
//   root -b -q -l 'tests/cross/root_ntuple_sums.C("file.root", "ntuple")'
//   root -b -q -l 'tests/cross/root_ntuple_sums.C("file.root", "ntuple", 5, 12)'   # entries [5, 12)
//
// Lines: `NTUPLE name entries`; then `FIELD ntuple field count sum` or
// `SKIP ntuple field reason`; `DONE`.

#include <ROOT/RDataFrame.hxx>

#include <cstdio>
#include <set>
#include <string>
#include <vector>

static bool number_type(const std::string &t)
{
   static const std::set<std::string> names = {
      "bool", "char", "signed char", "unsigned char", "short", "unsigned short", "int", "unsigned int",
      "long", "unsigned long", "long long", "unsigned long long", "float", "double", "std::byte",
      "std::int8_t", "std::uint8_t", "std::int16_t", "std::uint16_t", "std::int32_t", "std::uint32_t",
      "std::int64_t", "std::uint64_t", "Bool_t", "Char_t", "UChar_t", "Short_t", "UShort_t", "Int_t",
      "UInt_t", "Long64_t", "ULong64_t", "Float_t", "Double_t", "Double32_t", "Float16_t"};
   return names.count(t) > 0;
}

/// The element type of a vector of numbers, or "" if the type is not one.
static std::string element(const std::string &t)
{
   for (std::string wrap : {"std::vector<", "ROOT::VecOps::RVec<", "ROOT::RVec<"}) {
      if (t.rfind(wrap, 0) == 0 && t.back() == '>') {
         std::string inner = t.substr(wrap.size(), t.size() - wrap.size() - 1);
         return number_type(inner) ? inner : "";
      }
   }
   return "";
}

void root_ntuple_sums(const char *path, const char *name, Long64_t first = 0, Long64_t stop = -1)
{
   setvbuf(stdout, nullptr, _IOLBF, 0);
   ROOT::RDataFrame all(name, path);
   auto entries = *all.Count();
   printf("NTUPLE %s %llu\n", name, (unsigned long long)entries);
   if (first > (Long64_t)entries)
      first = entries;
   if (stop < 0 || stop > (Long64_t)entries)
      stop = entries;
   if (stop < first)
      stop = first;
   ROOT::RDF::RNode df = all;
   if (first != 0 || stop != (Long64_t)entries)
      df = all.Range(first, stop);
   struct Result {
      std::string field;
      ROOT::RDF::RResultPtr<double> count, sum;
   };
   std::vector<Result> results;
   int i = 0;
   for (auto &col : all.GetColumnNames()) {
      if (col.find('.') != std::string::npos || col.rfind("R_rdf", 0) == 0 || col.rfind("#", 0) == 0 ||
          col.rfind("_", 0) == 0)
         continue;
      std::string type = all.GetColumnType(col);
      std::string n, s;
      if (number_type(type)) {
         n = "1.0";
         s = "(double)" + col;
      } else if (!element(type).empty()) {
         n = "(double)" + col + ".size()";
         s = "double t = 0; for (auto x : " + col + ") t += (double)x; return t;";
      } else {
         printf("SKIP %s %s type=%s\n", name, col.c_str(), type.c_str());
         continue;
      }
      try {
         std::string cn = "rhz_n" + std::to_string(i), cs = "rhz_s" + std::to_string(i);
         ++i;
         auto d = df.Define(cn, n).Define(cs, s);
         results.push_back({col, d.Sum<double>(cn), d.Sum<double>(cs)});
      } catch (const std::exception &e) {
         printf("SKIP %s %s define-failed\n", name, col.c_str());
      }
   }
   for (auto &r : results)
      printf("FIELD %s %s %.0f %.17g\n", name, r.field.c_str(), *r.count, *r.sum);
   printf("DONE\n");
}

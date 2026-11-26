// Prints what ROOT reads from every object of a modeled class in a file,
// one fact per line: `KEY FIELD VALUE...`, floats exact (%.17g). Only
// what the file stores (cells as stored, with flow, x fastest; sums of
// squares; the stored statistics; axes; graph points; function
// parameters), so that a data model can be compared with it.
//
//   root -b -q -l 'tests/cross/root_dump.C("file.root")'

#include <TArray.h>
#include <TAxis.h>
#include <TF1.h>
#include <TFile.h>
#include <TFormula.h>
#include <TGraph.h>
#include <TGraphAsymmErrors.h>
#include <TGraphErrors.h>
#include <TH1.h>
#include <TH2.h>
#include <TH3.h>
#include <TKey.h>
#include <TList.h>
#include <TNamed.h>
#include <TObjString.h>
#include <TProfile.h>
#include <TProfile2D.h>
#include <TProfile3D.h>
#include <TSystem.h>

#include <cstdio>
#include <string>
#include <vector>

static void floats(const std::string &key, const std::string &field, const std::vector<double> &v)
{
   printf("%s %s", key.c_str(), field.c_str());
   for (double x : v)
      printf(" %.17g", x);
   printf("\n");
}

static void put_text(const std::string &key, const std::string &field, const char *v)
{
   printf("%s %s %s\n", key.c_str(), field.c_str(), v);
}

static std::vector<double> doubles(const TArrayD *a)
{
   return a ? std::vector<double>(a->GetArray(), a->GetArray() + a->GetSize()) : std::vector<double>();
}

static std::vector<double> points(const double *p, int n)
{
   return p ? std::vector<double>(p, p + n) : std::vector<double>();
}

static void dump_axis(const std::string &id, int d, const TAxis *a)
{
   std::string n = std::to_string(d);
   std::vector<double> e;
   for (int i = 1; i <= a->GetNbins(); ++i)
      e.push_back(a->GetBinLowEdge(i));
   e.push_back(a->GetBinUpEdge(a->GetNbins()));
   floats(id, "edges" + n, e);
   put_text(id, "axistitle" + n, a->GetTitle());
}

static void dump_hist(const std::string &id, TH1 *h)
{
   auto cells = dynamic_cast<TArray *>(h);
   std::vector<double> values;
   for (int i = 0; i < cells->GetSize(); ++i)
      values.push_back(cells->GetAt(i));
   floats(id, "values", values);
   floats(id, "sumw2", doubles(h->GetSumw2()));
   floats(id, "fEntries", {h->GetEntries()});
   int nstats = 4;
   if (h->InheritsFrom(TProfile3D::Class()))
      nstats = 13;
   else if (h->InheritsFrom(TProfile2D::Class()))
      nstats = 9;
   else if (h->InheritsFrom(TProfile::Class()))
      nstats = 6;
   else if (h->GetDimension() == 3)
      nstats = 11;
   else if (h->GetDimension() == 2)
      nstats = 7;
   double stats[TH1::kNstat] = {};
   h->GetStats(stats);
   floats(id, "stats", std::vector<double>(stats, stats + nstats));
   dump_axis(id, 0, h->GetXaxis());
   if (h->GetDimension() > 1)
      dump_axis(id, 1, h->GetYaxis());
   if (h->GetDimension() > 2)
      dump_axis(id, 2, h->GetZaxis());
   std::string fns;
   for (auto fn : *h->GetListOfFunctions())
      fns += std::string(" ") + fn->ClassName() + ":" + fn->GetName();
   printf("%s functions%s\n", id.c_str(), fns.c_str());

   // Profiles: the entries per cell, their sums of squared weights, the
   // error mode and the range of the profiled variable.
   auto profile = [&](auto *p, double lo, double hi) {
      std::vector<double> entries;
      for (int i = 0; i < p->GetNcells(); ++i)
         entries.push_back(p->GetBinEntries(i));
      floats(id, "binentries", entries);
      floats(id, "binsumw2", doubles(p->GetBinSumw2()));
      put_text(id, "errormode", p->GetErrorOption());
      floats(id, "range", {lo, hi});
   };
   if (auto p = dynamic_cast<TProfile *>(h))
      profile(p, p->GetYmin(), p->GetYmax());
   else if (auto p = dynamic_cast<TProfile2D *>(h))
      profile(p, p->GetZmin(), p->GetZmax());
   else if (auto p = dynamic_cast<TProfile3D *>(h))
      profile(p, p->GetTmin(), p->GetTmax());
}

static void dump_graph(const std::string &id, TGraph *g)
{
   int n = g->GetN();
   floats(id, "x", points(g->GetX(), n));
   floats(id, "y", points(g->GetY(), n));
   floats(id, "range", {g->GetMinimum(), g->GetMaximum()});
   if (auto e = dynamic_cast<TGraphAsymmErrors *>(g)) {
      floats(id, "exlow", points(e->GetEXlow(), n));
      floats(id, "exhigh", points(e->GetEXhigh(), n));
      floats(id, "eylow", points(e->GetEYlow(), n));
      floats(id, "eyhigh", points(e->GetEYhigh(), n));
   } else if (auto e = dynamic_cast<TGraphErrors *>(g)) {
      floats(id, "ex", points(e->GetEX(), n));
      floats(id, "ey", points(e->GetEY(), n));
   }
}

static void dump_function(const std::string &id, TF1 *f)
{
   int n = f->GetNpar();
   floats(id, "parameters", points(f->GetParameters(), n));
   floats(id, "parerrors", points(f->GetParErrors(), n));
   std::string names;
   for (int i = 0; i < n; ++i)
      names += std::string(i ? " " : "") + f->GetParName(i);
   put_text(id, "parnames", names.c_str());
   floats(id, "domain", {f->GetXmin(), f->GetXmax()});
   floats(id, "fit", {f->GetChisquare(), double(f->GetNDF())});
   put_text(id, "formula", f->GetFormula() ? f->GetFormula()->GetExpFormula().Data() : "");
}

void root_dump(const char *path)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("FILE error cannot-open\n");
      gSystem->Exit(1);
   }
   printf("FILE recovered %d\n", f->TestBit(TFile::kRecovered) ? 1 : 0);
   for (auto k : *f->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      std::string id = std::string(key->GetName()) + ";" + std::to_string(key->GetCycle());
      TObject *obj = key->ReadObj();
      if (!obj) {
         printf("%s error unreadable\n", id.c_str());
         continue;
      }
      put_text(id, "class", obj->ClassName());
      if (auto s = dynamic_cast<TObjString *>(obj)) {
         put_text(id, "string", s->GetString().Data());
         continue;
      }
      if (auto named = dynamic_cast<TNamed *>(obj)) {
         put_text(id, "name", named->GetName());
         put_text(id, "title", named->GetTitle());
      }
      if (auto h = dynamic_cast<TH1 *>(obj))
         dump_hist(id, h);
      else if (auto g = dynamic_cast<TGraph *>(obj))
         dump_graph(id, g);
      else if (auto fn = dynamic_cast<TF1 *>(obj))
         dump_function(id, fn);
   }
}
